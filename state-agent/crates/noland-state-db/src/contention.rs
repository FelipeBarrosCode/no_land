//! Process-local counters only: no user paths, SQL text, payloads or durable rows.
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

#[derive(Default)]
pub(super) struct DbLockTimings {
    writer_acquisitions: AtomicU64,
    writer_wait_ns: AtomicU64,
    monitoring_acquisitions: AtomicU64,
    monitoring_wait_ns: AtomicU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct DbContentionSnapshot {
    pub writer_lock_acquisitions: u64,
    pub writer_lock_wait_us: u64,
    pub monitoring_lock_acquisitions: u64,
    pub monitoring_lock_wait_us: u64,
}

fn add(counter: &AtomicU64, value: u64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_add(value))
    });
}

impl DbLockTimings {
    pub(super) fn record_writer(&self, elapsed: Duration) {
        add(&self.writer_acquisitions, 1);
        add(
            &self.writer_wait_ns,
            elapsed.as_nanos().min(u128::from(u64::MAX)) as u64,
        );
    }

    pub(super) fn record_monitoring(&self, elapsed: Duration) {
        add(&self.monitoring_acquisitions, 1);
        add(
            &self.monitoring_wait_ns,
            elapsed.as_nanos().min(u128::from(u64::MAX)) as u64,
        );
    }

    pub(super) fn snapshot(&self) -> DbContentionSnapshot {
        DbContentionSnapshot {
            writer_lock_acquisitions: self.writer_acquisitions.load(Ordering::Relaxed),
            writer_lock_wait_us: self.writer_wait_ns.load(Ordering::Relaxed) / 1000,
            monitoring_lock_acquisitions: self.monitoring_acquisitions.load(Ordering::Relaxed),
            monitoring_lock_wait_us: self.monitoring_wait_ns.load(Ordering::Relaxed) / 1000,
        }
    }
}

#[cfg(test)]
#[path = "contention_tests.rs"]
mod tests;
