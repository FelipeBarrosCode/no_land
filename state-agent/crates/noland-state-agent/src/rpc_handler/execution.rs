//! Bounded blocking dispatch, separate from the async Unix-socket I/O workers.
//!
//! The display lane is reserved for committed, read-only DB views. Writer-bound
//! reads, matching (which may backfill), and every mutation use the standard lane.
//! Acquire both limits asynchronously BEFORE spawning: blocked DB writers cannot
//! fill Tokio's blocking queue or consume the reserved display capacity.

use std::future::Future;
use std::sync::{Arc, OnceLock};

use noland_rpc::RpcRequest;
use noland_state_core::{Result, StateError};
use serde_json::Value;
use tokio::runtime::Handle;
use tokio::sync::Semaphore;

const DISPLAY_WORKERS: usize = 4;
const STANDARD_WORKERS: usize = 8;
const QUEUED_PER_LANE: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lane {
    Display,
    Standard,
}

impl Lane {
    fn for_request(request: &RpcRequest) -> Self {
        match request.method.as_str() {
            "GetHealth" | "ListApps" | "ListBackupCandidateIds" => Self::Display,
            "GetOperationStatus" | "GetBackupStatus" | "GetRestoreStatus" | "GetSealStatus"
                if request.params.get("projection").and_then(Value::as_str) == Some("progress") =>
            {
                Self::Display
            }
            _ => Self::Standard,
        }
    }
}

struct LaneBudget {
    workers: Arc<Semaphore>,
    admitted: Arc<Semaphore>,
}

impl LaneBudget {
    fn new(workers: usize, queued: usize) -> Self {
        Self {
            workers: Arc::new(Semaphore::new(workers)),
            admitted: Arc::new(Semaphore::new(workers + queued)),
        }
    }
}

struct ExecutionBudget {
    display: LaneBudget,
    standard: LaneBudget,
}

impl ExecutionBudget {
    fn new(display: usize, standard: usize, queued: usize) -> Self {
        Self {
            display: LaneBudget::new(display, queued),
            standard: LaneBudget::new(standard, queued),
        }
    }

    async fn run<F, Fut>(&self, lane: Lane, dispatch: F) -> Result<Value>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<Value>> + 'static,
    {
        let budget = match lane {
            Lane::Display => &self.display,
            Lane::Standard => &self.standard,
        };
        let reservation = Arc::clone(&budget.admitted)
            .try_acquire_owned()
            .map_err(|_| {
                StateError::Message(format!(
                    "RPC {lane:?} dispatch is at capacity; request was not dispatched"
                ))
            })?;
        let permit = Arc::clone(&budget.workers)
            .acquire_owned()
            .await
            .map_err(|_| StateError::Message("RPC dispatch is closed".into()))?;
        let runtime = Handle::current();
        tokio::task::spawn_blocking(move || {
            // The worker, not its awaiting socket task, owns these guards and
            // the captured request/agent. Dropping/aborting an awaiting handler
            // cannot release capacity or resources while a mutation still runs.
            // Queued handlers can be dropped before dispatch; started handlers
            // drain normally. A broken pipe is NOT an operation cancellation.
            let _reservation = reservation;
            let _permit = permit;
            runtime.block_on(dispatch())
        })
        .await
        .map_err(|error| StateError::Message(format!("RPC dispatch worker terminated: {error}")))?
    }
}

fn budget() -> &'static ExecutionBudget {
    static BUDGET: OnceLock<ExecutionBudget> = OnceLock::new();
    BUDGET.get_or_init(|| ExecutionBudget::new(DISPLAY_WORKERS, STANDARD_WORKERS, QUEUED_PER_LANE))
}

pub(super) async fn execute<F, Fut>(request: &RpcRequest, dispatch: F) -> Result<Value>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = Result<Value>> + 'static,
{
    budget().run(Lane::for_request(request), dispatch).await
}

#[cfg(test)]
#[path = "execution_tests.rs"]
mod tests;
