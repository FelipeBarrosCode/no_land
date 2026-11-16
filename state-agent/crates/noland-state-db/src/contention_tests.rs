use std::sync::mpsc;
use std::time::Duration;

use super::*;
use crate::StateDb;
use noland_state_core::{AppId, AppIdentity};

#[test]
fn wait_counters_accumulate_submicrosecond_samples_and_saturate() {
    let timing = DbLockTimings::default();
    timing.record_writer(Duration::from_nanos(700));
    timing.record_writer(Duration::from_nanos(700));
    timing.record_monitoring(Duration::from_nanos(2500));
    let snapshot = timing.snapshot();
    assert_eq!(snapshot.writer_lock_acquisitions, 2);
    assert_eq!(snapshot.writer_lock_wait_us, 1);
    assert_eq!(snapshot.monitoring_lock_acquisitions, 1);
    assert_eq!(snapshot.monitoring_lock_wait_us, 2);
    timing.writer_wait_ns.store(u64::MAX - 1, Ordering::Relaxed);
    timing.record_writer(Duration::from_nanos(10));
    assert_eq!(timing.writer_wait_ns.load(Ordering::Relaxed), u64::MAX);
}

#[test]
fn diagnostics_are_shared_by_clones_but_do_not_change_database_rows_or_generations() {
    let db = StateDb::open_in_memory().unwrap();
    let app = AppId::desktop("contention-metrics");
    db.upsert_app(&AppIdentity::new(app.clone(), "Synthetic"))
        .unwrap();
    let generation = db.capture_generation(&app).unwrap();
    let learned = db.learned_checkpoint_generation().unwrap();
    let clone = db.clone();
    let before = clone.contention_snapshot();
    db.unfinished_operation_count_for_health().unwrap();
    let after = clone.contention_snapshot();
    assert_eq!(
        after.monitoring_lock_acquisitions,
        before.monitoring_lock_acquisitions + 1
    );
    assert_eq!(
        after.writer_lock_acquisitions,
        before.writer_lock_acquisitions
    );
    assert_eq!(db.capture_generation(&app).unwrap(), generation);
    assert_eq!(db.learned_checkpoint_generation().unwrap(), learned);
    assert!(db.list_dirty_apps().unwrap().is_empty());
}

#[test]
fn a_waiting_writer_is_measured_without_holding_a_separate_diagnostic_lock() {
    let db = StateDb::open_in_memory().unwrap();
    let held = db.lock().unwrap();
    let before = db.contention_snapshot();
    let pending = db.clone();
    let (ready, waiting) = mpsc::channel();
    let (done, completed) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        ready.send(()).unwrap();
        pending.unfinished_operation_count().unwrap();
        done.send(()).unwrap();
    });
    waiting.recv().unwrap();
    let blocked = completed.recv_timeout(Duration::from_millis(20));
    // Diagnostics themselves must be readable while the SQLite writer is held.
    assert_eq!(db.contention_snapshot(), before);
    drop(held);
    worker.join().unwrap();
    assert!(blocked.is_err());
    let after = db.contention_snapshot();
    assert_eq!(
        after.writer_lock_acquisitions,
        before.writer_lock_acquisitions + 1
    );
    assert!(after.writer_lock_wait_us > before.writer_lock_wait_us);
}
