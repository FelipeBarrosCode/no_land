use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Process-local counters. The agent exports these via GetHealth.
#[derive(Debug, Default)]
pub struct Metrics {
    pub process_events_total: AtomicU64,
    pub filesystem_events_total: AtomicU64,
    pub filesystem_events_dropped_total: AtomicU64,
    pub events_coalesced_total: AtomicU64,
    pub reconciliations_total: AtomicU64,
    pub apps_discovered_total: AtomicU64,
    pub unknown_paths: AtomicU64,
    pub dirty_apps: AtomicU64,
    pub hash_bytes_total: AtomicU64,
    pub chunks_created_total: AtomicU64,
    pub chunks_reused_total: AtomicU64,
    pub pack_bytes_created_total: AtomicU64,
    pub upload_bytes_total: AtomicU64,
    pub restore_rollbacks_total: AtomicU64,
    pub provider_errors_total: AtomicU64,
}

impl Metrics {
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn inc(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn add(counter: &AtomicU64, n: u64) {
        counter.fetch_add(n, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            process_events_total: self.process_events_total.load(Ordering::Relaxed),
            filesystem_events_total: self.filesystem_events_total.load(Ordering::Relaxed),
            filesystem_events_dropped_total: self
                .filesystem_events_dropped_total
                .load(Ordering::Relaxed),
            events_coalesced_total: self.events_coalesced_total.load(Ordering::Relaxed),
            reconciliations_total: self.reconciliations_total.load(Ordering::Relaxed),
            apps_discovered_total: self.apps_discovered_total.load(Ordering::Relaxed),
            unknown_paths: self.unknown_paths.load(Ordering::Relaxed),
            dirty_apps: self.dirty_apps.load(Ordering::Relaxed),
            hash_bytes_total: self.hash_bytes_total.load(Ordering::Relaxed),
            chunks_created_total: self.chunks_created_total.load(Ordering::Relaxed),
            chunks_reused_total: self.chunks_reused_total.load(Ordering::Relaxed),
            pack_bytes_created_total: self.pack_bytes_created_total.load(Ordering::Relaxed),
            upload_bytes_total: self.upload_bytes_total.load(Ordering::Relaxed),
            restore_rollbacks_total: self.restore_rollbacks_total.load(Ordering::Relaxed),
            provider_errors_total: self.provider_errors_total.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MetricsSnapshot {
    pub process_events_total: u64,
    pub filesystem_events_total: u64,
    pub filesystem_events_dropped_total: u64,
    pub events_coalesced_total: u64,
    pub reconciliations_total: u64,
    pub apps_discovered_total: u64,
    pub unknown_paths: u64,
    pub dirty_apps: u64,
    pub hash_bytes_total: u64,
    pub chunks_created_total: u64,
    pub chunks_reused_total: u64,
    pub pack_bytes_created_total: u64,
    pub upload_bytes_total: u64,
    pub restore_rollbacks_total: u64,
    pub provider_errors_total: u64,
}
