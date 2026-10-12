//! Timing and structured concurrency for provisioning dependency lanes.
use std::{future::Future, time::Instant};

use crate::errors::AppResult;

pub fn measure<T>(
    stage: &'static str,
    work: impl Future<Output = AppResult<T>>,
) -> impl Future<Output = AppResult<T>> {
    // Box before constructing the timing future. An async fn would retain
    // the entire concrete child future inline in its own state machine.
    let work = Box::pin(work);
    async move {
        let started = Instant::now();
        tracing::info!(stage, "provisioning stage started");
        let result = work.await;
        tracing::info!(
            stage,
            duration_ms = started.elapsed().as_millis() as u64,
            success = result.is_ok(),
            "provisioning stage finished"
        );
        result
    }
}

/// Drain both lanes even on failure. A retry must not race orphaned remote
/// commands: dropping a blocking SSH future does not stop those commands.
pub fn join_lanes<A, B>(
    display: impl Future<Output = AppResult<A>>,
    network: impl Future<Output = AppResult<B>>,
) -> impl Future<Output = AppResult<(A, B)>> {
    let display = Box::pin(measure("display_audio", display));
    let network = Box::pin(measure("wireguard_agents", network));
    async move {
        let (display, network) = tokio::join!(display, network);
        Ok((display?, network?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::AppError;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    fn large_stage() -> impl Future<Output = AppResult<u8>> {
        async {
            let payload = [42u8; 256 * 1024];
            tokio::task::yield_now().await;
            Ok(std::hint::black_box(payload)[0])
        }
    }

    #[test]
    fn wrappers_do_not_embed_large_stage_state_on_the_worker_stack() {
        assert!(std::mem::size_of_val(&large_stage()) >= 256 * 1024);
        let timed = measure("large", large_stage());
        assert!(std::mem::size_of_val(&timed) < 1024);
        let joined = join_lanes(large_stage(), large_stage());
        assert!(std::mem::size_of_val(&joined) < 1024);
    }

    #[tokio::test]
    async fn independent_lanes_start_together() {
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let other = barrier.clone();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            join_lanes(
                async move {
                    barrier.wait().await;
                    Ok(1)
                },
                async move {
                    other.wait().await;
                    Ok(2)
                },
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result, (1, 2));
    }

    #[tokio::test]
    async fn failure_does_not_abandon_other_lane() {
        let completed = Arc::new(AtomicBool::new(false));
        let observed = completed.clone();
        let result = join_lanes(async { Err::<(), _>(AppError::Cancelled) }, async move {
            tokio::task::yield_now().await;
            observed.store(true, Ordering::SeqCst);
            Ok(())
        })
        .await;
        assert!(result.is_err());
        assert!(completed.load(Ordering::SeqCst));
    }
}
