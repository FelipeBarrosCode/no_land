//! One joined blocking worker per occupied execution lane. Socket/I/O workers
//! never run the synchronous discovery, indexing or cancellation callbacks.
use std::panic::{catch_unwind, AssertUnwindSafe};

use super::*;

pub(super) struct Supervision {
    operation_id: Uuid,
    execution_id: Uuid,
    state: Arc<Mutex<ManagerState>>,
    armed: bool,
}

impl Supervision {
    pub(super) fn new(
        operation_id: Uuid,
        execution_id: Uuid,
        state: Arc<Mutex<ManagerState>>,
    ) -> Self {
        Self {
            operation_id,
            execution_id,
            state,
            armed: true,
        }
    }

    pub(super) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for Supervision {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Some(operation) = self
            .state
            .lock()
            .running
            .get_mut(&self.operation_id)
            .filter(|operation| operation.execution_id == self.execution_id)
        {
            operation.cancel_requested = true;
            let _ = operation.cancel.send(true);
        }
    }
}

pub(super) async fn cancel_queued<F, K, C>(
    running: Arc<RunningGuard>,
    callbacks: Arc<tokio::sync::Semaphore>,
    future: F,
    on_cancelled: K,
    on_terminated: C,
) where
    F: Future<Output = ()> + Send + 'static,
    K: FnOnce() + Send + 'static,
    C: FnOnce(String) + Send + 'static,
{
    // Cancelled queued requests cannot create an unbounded blocking-thread
    // burst, and do not need the execution lane held by a running transfer.
    let permit = callbacks
        .acquire_owned()
        .await
        .expect("cancellation callback budget is never closed");
    let worker = tokio::task::spawn_blocking(move || {
        let _running = running;
        let _permit = permit;
        let result = catch_unwind(AssertUnwindSafe(|| {
            // The unpolled future may own synchronous cleanup. Keep its Drop
            // off the socket runtime too, before reporting cancellation.
            drop(future);
            on_cancelled();
        }));
        if let Err(payload) = result {
            on_terminated(panic_reason(payload.as_ref()));
        }
    });
    if let Err(error) = worker.await {
        tracing::error!(%error, "queued cancellation callback terminated");
    }
}

pub(super) async fn run<F, K, C>(
    running: Arc<RunningGuard>,
    permit: tokio::sync::OwnedMutexGuard<()>,
    mut cancellation: watch::Receiver<bool>,
    future: F,
    on_cancelled: K,
    on_terminated: C,
) where
    F: Future<Output = ()> + Send + 'static,
    K: FnOnce() + Send + 'static,
    C: FnOnce(String) + Send + 'static,
{
    let runtime = tokio::runtime::Handle::current();
    // Acquiring the lane precedes spawn_blocking: at most one managed worker
    // per lane can be queued/running. Keep BOTH the lane and running lease in
    // that worker through cancellation, finalization and terminal callbacks.
    let worker = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            runtime.block_on(async {
                tokio::pin!(future);
                tokio::select! {
                    biased;
                    _ = wait_for_cancellation(&mut cancellation) => {
                        let finalizing = running.state.lock().running
                            .get(&running.operation_id)
                            .is_some_and(|operation| operation.finalizers != 0);
                        if finalizing {
                            // Authenticated data is already durable: finish
                            // exact claim release and terminal publication.
                            future.await;
                            WorkerOutcome::Completed
                        } else {
                            WorkerOutcome::Cancelled
                        }
                    }
                    _ = &mut future => WorkerOutcome::Completed,
                }
            })
        }));
        match outcome {
            Ok(WorkerOutcome::Completed) => {}
            Ok(WorkerOutcome::Cancelled) => on_cancelled(),
            Err(payload) => {
                on_terminated(panic_reason(payload.as_ref()));
            }
        }
        drop(_permit);
        drop(running);
    });
    // Do not select-drop this join on cancellation. The worker observes the
    // same watch signal and owns the execution resources until it has drained.
    if let Err(error) = worker.await {
        tracing::error!(%error, "managed terminal callback terminated");
    }
}

fn panic_reason(payload: &(dyn std::any::Any + Send)) -> String {
    let reason = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied())
        .unwrap_or("non-string panic");
    format!("background operation terminated unexpectedly: {reason}")
}

#[cfg(test)]
#[path = "execution/tests.rs"]
mod tests;
