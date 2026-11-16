use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc, Condvar, Mutex as StdMutex,
};
use std::time::Duration;

use super::*;

#[derive(Default)]
struct Blocker {
    released: StdMutex<bool>,
    wake: Condvar,
}

impl Blocker {
    fn wait(&self) {
        let guard = self.released.lock().unwrap();
        let (guard, result) = self
            .wake
            .wait_timeout_while(guard, Duration::from_secs(5), |released| !*released)
            .unwrap();
        assert!(
            *guard && !result.timed_out(),
            "test blocker was not released"
        );
    }

    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.wake.notify_all();
    }
}

#[test]
fn synchronous_managed_work_does_not_occupy_tokio_io_workers() {
    for workers in [1, 2] {
        let blocker = Arc::new(Blocker::default());
        let work_blocker = Arc::clone(&blocker);
        let (ready, observed) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(workers)
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let manager = OperationManager::default();
                let mut starts = Vec::new();
                for lane in [OperationLane::Control, OperationLane::Transfer] {
                    let work_blocker = Arc::clone(&work_blocker);
                    let (started, receiver) = tokio::sync::oneshot::channel();
                    assert!(manager.spawn_cancellable_on_lane(
                        Uuid::new_v4(),
                        lane,
                        async move {
                            let _ = started.send(());
                            work_blocker.wait();
                        },
                        || {},
                        |_| panic!("worker must complete"),
                    ));
                    starts.push(receiver);
                }
                for started in starts {
                    started.await.unwrap();
                }
                // A real timer needs an available I/O driver; checking only
                // synchronous manager state on block_on's caller is insufficient.
                tokio::time::sleep(Duration::from_millis(1)).await;
                ready.send(()).unwrap();
                while !manager.running_snapshots().is_empty() {
                    tokio::task::yield_now().await;
                }
            });
        });
        let result = observed.recv_timeout(Duration::from_secs(3));
        blocker.release();
        thread.join().unwrap();
        assert!(
            result.is_ok(),
            "{workers}-worker runtime could not drive I/O until blocking work ended"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_keeps_lane_and_running_state_until_synchronous_work_drains() {
    let manager = OperationManager::default();
    let id = Uuid::new_v4();
    let blocker = Arc::new(Blocker::default());
    let blocked = Arc::clone(&blocker);
    let (entered, started) = tokio::sync::oneshot::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    let notified = Arc::clone(&cancelled);
    assert!(manager.spawn_cancellable_on_lane(
        id,
        OperationLane::Transfer,
        async move {
            entered.send(()).unwrap();
            blocked.wait();
            std::future::pending::<()>().await;
        },
        move || notified.store(true, Ordering::SeqCst),
        |_| panic!("worker must not panic"),
    ));
    started.await.unwrap();
    assert_eq!(manager.cancel(id), CancelOutcome::Requested);
    tokio::task::yield_now().await;
    assert!(manager.is_running(id));
    assert!(manager.try_transfer_guard().is_none());
    assert!(!cancelled.load(Ordering::SeqCst));
    blocker.release();
    tokio::time::timeout(Duration::from_secs(3), async {
        while manager.is_running(id) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(cancelled.load(Ordering::SeqCst));
    assert!(manager.try_transfer_guard().is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn aborting_supervision_cannot_detach_work_and_release_its_lane_early() {
    let manager = OperationManager::default();
    let id = Uuid::new_v4();
    let blocker = Arc::new(Blocker::default());
    let blocked = Arc::clone(&blocker);
    let (entered, started) = tokio::sync::oneshot::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    let notified = Arc::clone(&cancelled);
    assert!(manager.spawn_cancellable_on_lane(
        id,
        OperationLane::Transfer,
        async move {
            entered.send(()).unwrap();
            blocked.wait();
            std::future::pending::<()>().await;
        },
        move || notified.store(true, Ordering::SeqCst),
        |_| panic!("worker must not panic"),
    ));
    started.await.unwrap();
    manager.state.lock().running[&id]
        .supervisor_abort
        .as_ref()
        .unwrap()
        .abort();
    tokio::task::yield_now().await;
    assert!(manager.is_running(id));
    assert!(manager.try_transfer_guard().is_none());
    blocker.release();
    tokio::time::timeout(Duration::from_secs(3), async {
        while manager.is_running(id) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(cancelled.load(Ordering::SeqCst));
    assert!(manager.try_transfer_guard().is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn aborting_supervision_still_drains_durable_finalization_instead_of_cancelling_it() {
    let manager = OperationManager::default();
    let id = Uuid::new_v4();
    let worker_manager = manager.clone();
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, finish) = tokio::sync::oneshot::channel();
    let completed = Arc::new(AtomicBool::new(false));
    let worker_completed = Arc::clone(&completed);
    let cancelled = Arc::new(AtomicBool::new(false));
    let notified = Arc::clone(&cancelled);
    assert!(manager.spawn_cancellable_on_lane(
        id,
        OperationLane::Transfer,
        async move {
            let _finalizing = worker_manager.finalization_scope(id);
            entered.send(()).unwrap();
            finish.await.unwrap();
            worker_completed.store(true, Ordering::SeqCst);
        },
        move || notified.store(true, Ordering::SeqCst),
        |_| panic!("worker must not panic"),
    ));
    started.await.unwrap();
    manager.state.lock().running[&id]
        .supervisor_abort
        .as_ref()
        .unwrap()
        .abort();
    tokio::task::yield_now().await;
    assert!(manager.is_running(id));
    assert!(manager.try_transfer_guard().is_none());
    assert!(!completed.load(Ordering::SeqCst));
    assert!(!cancelled.load(Ordering::SeqCst));
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while manager.is_running(id) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(completed.load(Ordering::SeqCst));
    assert!(!cancelled.load(Ordering::SeqCst));
    assert!(manager.try_transfer_guard().is_some());
}

#[tokio::test(flavor = "current_thread")]
async fn a_blocking_queued_cancellation_callback_does_not_occupy_io_or_start_the_transfer() {
    let manager = OperationManager::default();
    let first = Uuid::new_v4();
    let queued = Uuid::new_v4();
    let (first_entered, first_started) = tokio::sync::oneshot::channel();
    let (release_first, first_finish) = tokio::sync::oneshot::channel();
    assert!(manager.spawn(
        first,
        async move {
            first_entered.send(()).unwrap();
            first_finish.await.unwrap();
        },
        |_| panic!("first operation must not panic"),
    ));
    first_started.await.unwrap();
    let blocker = Arc::new(Blocker::default());
    let blocked = Arc::clone(&blocker);
    let (entered, started) = tokio::sync::oneshot::channel();
    let ran = Arc::new(AtomicBool::new(false));
    let worker_ran = Arc::clone(&ran);
    assert!(manager.spawn_cancellable(
        queued,
        async move {
            worker_ran.store(true, Ordering::SeqCst);
        },
        move || {
            entered.send(()).unwrap();
            blocked.wait();
        },
        |_| panic!("queued operation must not panic"),
    ));
    let began = Instant::now();
    assert_eq!(manager.cancel(queued), CancelOutcome::Requested);
    started.await.unwrap();
    assert!(began.elapsed() < Duration::from_secs(3));
    tokio::time::sleep(Duration::from_millis(1)).await;
    assert!(manager.is_running(queued));
    assert!(!ran.load(Ordering::SeqCst));
    blocker.release();
    release_first.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while manager.is_running(first) || manager.is_running(queued) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!ran.load(Ordering::SeqCst));
}

#[tokio::test(flavor = "current_thread")]
async fn queued_cancellation_callbacks_have_a_separate_bounded_worker_budget() {
    let manager = OperationManager::default();
    let first = Uuid::new_v4();
    let (entered_first, first_started) = tokio::sync::oneshot::channel();
    let (release_first, finish_first) = tokio::sync::oneshot::channel();
    assert!(manager.spawn(
        first,
        async move {
            entered_first.send(()).unwrap();
            finish_first.await.unwrap();
        },
        |_| panic!("first operation must not panic"),
    ));
    first_started.await.unwrap();

    let blocker = Arc::new(Blocker::default());
    let active = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    let callbacks = Arc::new(AtomicUsize::new(0));
    let ran = Arc::new(AtomicBool::new(false));
    let (entered, mut started) = tokio::sync::mpsc::unbounded_channel();
    let mut queued = Vec::new();
    for _ in 0..6 {
        let id = Uuid::new_v4();
        queued.push(id);
        let blocked = Arc::clone(&blocker);
        let active = Arc::clone(&active);
        let maximum = Arc::clone(&maximum);
        let callbacks = Arc::clone(&callbacks);
        let worker_ran = Arc::clone(&ran);
        let entered = entered.clone();
        assert!(manager.spawn_cancellable(
            id,
            async move {
                worker_ran.store(true, Ordering::SeqCst);
            },
            move || {
                let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                maximum.fetch_max(count, Ordering::SeqCst);
                entered.send(()).unwrap();
                blocked.wait();
                callbacks.fetch_add(1, Ordering::SeqCst);
                active.fetch_sub(1, Ordering::SeqCst);
            },
            |_| panic!("queued operation must not panic"),
        ));
        assert_eq!(manager.cancel(id), CancelOutcome::Requested);
    }
    started.recv().await.unwrap();
    started.recv().await.unwrap();
    assert_eq!(manager.cancellation_callbacks.available_permits(), 0);
    assert_eq!(active.load(Ordering::SeqCst), 2);
    assert_eq!(maximum.load(Ordering::SeqCst), 2);
    assert!(started.try_recv().is_err());
    assert!(!ran.load(Ordering::SeqCst));
    blocker.release();
    tokio::time::timeout(Duration::from_secs(3), async {
        while queued.iter().any(|id| manager.is_running(*id)) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(callbacks.load(Ordering::SeqCst), 6);
    assert_eq!(maximum.load(Ordering::SeqCst), 2);
    assert!(!ran.load(Ordering::SeqCst));
    release_first.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while manager.is_running(first) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn abort_before_the_supervisors_first_poll_removes_its_registration() {
    let manager = OperationManager::default();
    let id = Uuid::new_v4();
    assert!(manager.spawn(id, std::future::pending(), |_| {}));
    // No await/yield occurs between registration and abort.
    manager.state.lock().running[&id]
        .supervisor_abort
        .as_ref()
        .unwrap()
        .abort();
    tokio::time::timeout(Duration::from_secs(3), async {
        while manager.is_running(id) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(manager.cancel(id), CancelOutcome::NotRunning);
    assert!(manager.spawn(id, async {}, |_| {}));
    tokio::time::timeout(Duration::from_secs(3), async {
        while manager.is_running(id) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn stale_supervision_and_running_leases_cannot_cancel_or_remove_a_reused_id() {
    let manager = OperationManager::default();
    let id = Uuid::new_v4();
    let (first_release, finish_first) = tokio::sync::oneshot::channel();
    assert!(manager.spawn(
        id,
        async move {
            finish_first.await.unwrap();
        },
        |_| {},
    ));
    let old_execution = manager.state.lock().running[&id].execution_id;
    let stale_supervisor = Supervision::new(id, old_execution, Arc::clone(&manager.state));
    let stale_lease = RunningGuard {
        operation_id: id,
        execution_id: old_execution,
        state: Arc::clone(&manager.state),
    };
    first_release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while manager.is_running(id) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    let (second_release, finish_second) = tokio::sync::oneshot::channel();
    assert!(manager.spawn(
        id,
        async move {
            finish_second.await.unwrap();
        },
        |_| {},
    ));
    let replacement = manager.state.lock().running[&id].execution_id;
    assert_ne!(replacement, old_execution);
    drop(stale_supervisor);
    drop(stale_lease);
    assert!(manager.is_running(id));
    assert!(!manager.cancel_requested(id));
    assert_eq!(manager.state.lock().running[&id].execution_id, replacement);
    second_release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while manager.is_running(id) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

struct BlockingDrop {
    entered: Option<tokio::sync::oneshot::Sender<()>>,
    blocker: Arc<Blocker>,
}

impl Drop for BlockingDrop {
    fn drop(&mut self) {
        self.entered.take().unwrap().send(()).unwrap();
        self.blocker.wait();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn cancelling_unstarted_work_drops_its_captured_resources_off_the_io_thread() {
    let manager = OperationManager::default();
    let first = Uuid::new_v4();
    let queued = Uuid::new_v4();
    let (first_entered, first_started) = tokio::sync::oneshot::channel();
    let (release_first, finish_first) = tokio::sync::oneshot::channel();
    assert!(manager.spawn(
        first,
        async move {
            first_entered.send(()).unwrap();
            finish_first.await.unwrap();
        },
        |_| {},
    ));
    first_started.await.unwrap();
    let blocker = Arc::new(Blocker::default());
    let (entered, started) = tokio::sync::oneshot::channel();
    let capture = BlockingDrop {
        entered: Some(entered),
        blocker: Arc::clone(&blocker),
    };
    let cancelled = Arc::new(AtomicBool::new(false));
    let notified = Arc::clone(&cancelled);
    assert!(manager.spawn_cancellable(
        queued,
        async move {
            let _capture = capture;
            panic!("queued future must never be polled");
        },
        move || notified.store(true, Ordering::SeqCst),
        |_| panic!("unused capture must drop without panicking"),
    ));
    let began = Instant::now();
    assert_eq!(manager.cancel(queued), CancelOutcome::Requested);
    started.await.unwrap();
    assert!(began.elapsed() < Duration::from_secs(3));
    tokio::time::sleep(Duration::from_millis(1)).await;
    assert!(manager.is_running(queued));
    assert!(!cancelled.load(Ordering::SeqCst));
    blocker.release();
    release_first.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while manager.is_running(first) || manager.is_running(queued) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(cancelled.load(Ordering::SeqCst));
}
