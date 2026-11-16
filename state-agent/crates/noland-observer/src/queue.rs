use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use noland_state_core::metrics::Metrics;
use noland_state_core::{
    EbpfFilesystemFact, EbpfProcessFact, FilesystemEvent, FsEventKind, ProcessEvent,
};
use parking_lot::Mutex;

use crate::ProcessGenerationProof;

#[derive(Debug, Clone)]
pub enum QueuedEvent {
    Process(ProcessEvent),
    Filesystem(FilesystemEvent),
    EbpfProcess(EbpfProcessFact),
    EbpfFilesystem(EbpfFilesystemFact),
    FilesystemGap { pid: i32 },
}

/// Acknowledging this snapshot cannot clear a newer loss reported by a producer.
#[derive(Debug, Clone, Copy)]
pub struct QueueLoss {
    pub dropped: u64,
}

#[derive(Debug, Clone)]
pub struct ObservedQueuedEvent {
    pub event: QueuedEvent,
    pub process_proof: Option<ProcessGenerationProof>,
}

/// Loss and queue coverage sampled atomically with detaching a FIFO prefix.
#[derive(Debug, Clone, Copy)]
pub struct QueueWatermark {
    loss: Option<QueueLoss>,
    complete: bool,
}

pub struct EventQueue {
    cap: usize,
    inner: Mutex<Inner>,
    metrics: Arc<Metrics>,
    notify: tokio::sync::Notify,
}

struct Inner {
    events: VecDeque<ObservedQueuedEvent>,
    last_write: HashMap<(i32, FsEventKind, std::path::PathBuf), usize>,
    dropped: u64,
    loss_detected: bool,
}

impl EventQueue {
    pub fn new(cap: usize, metrics: Arc<Metrics>) -> Self {
        Self {
            cap,
            metrics,
            notify: tokio::sync::Notify::new(),
            inner: Mutex::new(Inner {
                events: VecDeque::with_capacity(cap.min(4096)),
                last_write: HashMap::new(),
                dropped: 0,
                loss_detected: false,
            }),
        }
    }

    pub fn push(&self, event: QueuedEvent) {
        self.push_observed(event, None);
    }

    pub fn push_observed(&self, event: QueuedEvent, process_proof: Option<ProcessGenerationProof>) {
        let mut inner = self.inner.lock();
        // A process boundary (including PID reuse), rename, or deletion ends a
        // coalescing run. Never merge two apps' identical namespace-local paths.
        if write_identity(&event).is_none() {
            inner.last_write.clear();
        }
        if let Some((_, _, at)) = filesystem_identity(&event) {
            if let Some(key) = write_identity(&event) {
                if let Some(idx) = inner.last_write.get(&key).copied() {
                    if inner.events.get(idx).is_some_and(|existing| {
                        match (&existing.process_proof, &process_proof) {
                            (Some(left), Some(right)) => left.same_process(right),
                            (None, None) => true,
                            _ => false,
                        }
                    }) {
                        match inner.events.get_mut(idx).map(|entry| &mut entry.event) {
                            Some(QueuedEvent::Filesystem(existing)) => existing.at = at,
                            Some(QueuedEvent::EbpfFilesystem(existing)) => existing.at = at,
                            _ => {}
                        }
                        Metrics::inc(&self.metrics.events_coalesced_total);
                        drop(inner);
                        self.notify.notify_one();
                        return;
                    }
                }
            }
        }

        if inner.events.len() >= self.cap {
            // Drop lowest-value events first: read-only filesystem telemetry.
            if let Some(pos) = inner.events.iter().position(|event| {
                filesystem_identity(&event.event)
                    .is_some_and(|(kind, _, _)| kind.is_read() && !kind.is_mutation())
            }) {
                inner.events.remove(pos);
                inner.last_write.clear();
                inner.dropped += 1;
                inner.loss_detected = true;
                Metrics::inc(&self.metrics.filesystem_events_dropped_total);
            } else if let Some(pos) = inner
                .events
                .iter()
                .position(|queued| filesystem_identity(&queued.event).is_some())
            {
                inner.events.remove(pos);
                inner.last_write.clear();
                inner.dropped += 1;
                inner.loss_detected = true;
                Metrics::inc(&self.metrics.filesystem_events_dropped_total);
            } else if is_process_event(&event) {
                // A process fact is more valuable than an older process fact only when
                // no filesystem telemetry remains to evict.
                inner.events.pop_front();
                inner.dropped += 1;
                inner.loss_detected = true;
            } else {
                // Preserve the process identity stream. The incoming filesystem event
                // is deliberately discarded instead of orphaning later file facts.
                inner.dropped += 1;
                inner.loss_detected = true;
                Metrics::inc(&self.metrics.filesystem_events_dropped_total);
                drop(inner);
                self.notify.notify_one();
                return;
            }
        }
        let write_key = write_identity(&event);
        let idx = inner.events.len();
        if let Some(key) = write_key {
            inner.last_write.insert(key, idx);
        }
        inner.events.push_back(ObservedQueuedEvent {
            event,
            process_proof,
        });
        drop(inner);
        self.notify.notify_one();
    }

    pub async fn notified(&self) {
        let notified = self.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        // Notify permits coalesce. The queue, not the number of permits, is the
        // source of truth when a bounded consumer leaves another prefix behind.
        if self.has_pending() {
            return;
        }
        notified.await;
    }

    /// Atomically detaches every event present at the flush-start watermark.
    pub fn drain(&self) -> Vec<QueuedEvent> {
        self.drain_prefix(usize::MAX)
    }

    /// Detaches only a FIFO prefix; never drain everything and then take a limit.
    pub fn drain_prefix(&self, limit: usize) -> Vec<QueuedEvent> {
        self.drain_observed_prefix(limit)
            .0
            .into_iter()
            .map(|entry| entry.event)
            .collect()
    }

    pub fn drain_observed_prefix(
        &self,
        limit: usize,
    ) -> (Vec<ObservedQueuedEvent>, QueueWatermark) {
        let mut inner = self.inner.lock();
        let count = limit.min(inner.events.len());
        let watermark = QueueWatermark {
            loss: inner.loss_detected.then_some(QueueLoss {
                dropped: inner.dropped,
            }),
            complete: count == inner.events.len(),
        };
        // Offsets change on prefix removal. Starting a fresh coalescing run is
        // conservative and also preserves process/rename/delete boundaries.
        inner.last_write.clear();
        let events = inner.events.drain(..count).collect();
        let pending = !inner.events.is_empty();
        drop(inner);
        if pending {
            self.notify.notify_one();
        }
        (events, watermark)
    }

    /// Restore an unprocessed detached prefix ahead of concurrently queued facts.
    /// Retried facts are never evicted. If producers filled its former space,
    /// discard newer arrivals explicitly as loss rather than exceeding the cap.
    pub fn restore_front(&self, events: impl IntoIterator<Item = QueuedEvent>) {
        self.restore_observed_front(events.into_iter().map(|event| ObservedQueuedEvent {
            event,
            process_proof: None,
        }));
    }

    pub fn restore_observed_front(&self, events: impl IntoIterator<Item = ObservedQueuedEvent>) {
        let events = events.into_iter().collect::<VecDeque<_>>();
        if events.is_empty() {
            return;
        }
        let mut inner = self.inner.lock();
        inner.last_write.clear();
        let keep = self.cap.saturating_sub(events.len());
        while inner.events.len() > keep {
            let event = inner.events.pop_back().expect("queue is nonempty");
            inner.dropped = inner.dropped.saturating_add(1);
            inner.loss_detected = true;
            if filesystem_identity(&event.event).is_some() {
                Metrics::inc(&self.metrics.filesystem_events_dropped_total);
            }
        }
        let mut restored = events;
        restored.append(&mut inner.events);
        inner.events = restored;
        drop(inner);
        self.notify.notify_one();
    }

    pub fn has_pending(&self) -> bool {
        let inner = self.inner.lock();
        !inner.events.is_empty() || inner.loss_detected
    }

    pub fn has_queued_events(&self) -> bool {
        !self.inner.lock().events.is_empty()
    }

    pub fn loss_snapshot(&self) -> Option<QueueLoss> {
        let inner = self.inner.lock();
        inner.loss_detected.then_some(QueueLoss {
            dropped: inner.dropped,
        })
    }

    /// Call only after the snapshot's durable reconciliation fence succeeded.
    /// A bounded prefix cannot acknowledge loss with an unprocessed FIFO suffix:
    /// that suffix may still straddle the missing process-generation boundary.
    pub fn acknowledge_loss(&self, loss: QueueLoss) -> bool {
        let mut inner = self.inner.lock();
        if inner.dropped != loss.dropped || !inner.events.is_empty() {
            return false;
        }
        inner.loss_detected = false;
        true
    }

    /// Completion of a full detached watermark is independent of newer queued
    /// arrivals. A newer loss generation, unlike an ordinary arrival, stays sticky.
    pub fn acknowledge_watermark_loss(&self, loss: QueueLoss, watermark: QueueWatermark) -> bool {
        let mut inner = self.inner.lock();
        if !watermark.complete
            || watermark.loss.map(|covered| covered.dropped) != Some(loss.dropped)
            || inner.dropped != loss.dropped
        {
            return false;
        }
        inner.loss_detected = false;
        true
    }

    pub fn report_loss(&self, count: u64) {
        if count == 0 {
            return;
        }
        let mut inner = self.inner.lock();
        inner.dropped = inner.dropped.saturating_add(count);
        inner.loss_detected = true;
        Metrics::add(&self.metrics.filesystem_events_dropped_total, count);
        drop(inner);
        self.notify.notify_one();
    }

    pub fn take_loss_flag(&self) -> bool {
        let mut inner = self.inner.lock();
        let lost = inner.loss_detected;
        inner.loss_detected = false;
        lost
    }

    pub fn dropped(&self) -> u64 {
        self.inner.lock().dropped
    }
}

fn is_process_event(event: &QueuedEvent) -> bool {
    matches!(event, QueuedEvent::Process(_) | QueuedEvent::EbpfProcess(_))
}

fn filesystem_identity(
    event: &QueuedEvent,
) -> Option<(FsEventKind, &std::path::Path, chrono::DateTime<chrono::Utc>)> {
    match event {
        QueuedEvent::Filesystem(fact) => Some((fact.kind, &fact.path, fact.at)),
        QueuedEvent::EbpfFilesystem(fact) => Some((fact.kind, &fact.path, fact.at)),
        QueuedEvent::Process(_)
        | QueuedEvent::EbpfProcess(_)
        | QueuedEvent::FilesystemGap { .. } => None,
    }
}

fn write_identity(event: &QueuedEvent) -> Option<(i32, FsEventKind, std::path::PathBuf)> {
    let (kind, path, _) = filesystem_identity(event)?;
    if !matches!(kind, FsEventKind::Write | FsEventKind::Truncate) {
        return None;
    }
    let pid = match event {
        QueuedEvent::Filesystem(event) => event.pid,
        QueuedEvent::EbpfFilesystem(event) => event.process_id(),
        _ => return None,
    };
    Some((pid, kind, path.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use noland_state_core::metrics::Metrics;
    use std::path::PathBuf;
    use std::sync::Arc;

    #[test]
    fn reports_external_loss_to_recovery_and_metrics() {
        let metrics = Arc::new(Metrics::default());
        let q = EventQueue::new(16, metrics.clone());
        q.report_loss(7);
        assert_eq!(q.dropped(), 7);
        assert!(q.take_loss_flag());
        assert!(!q.take_loss_flag());
        assert_eq!(
            metrics
                .filesystem_events_dropped_total
                .load(std::sync::atomic::Ordering::Relaxed),
            7
        );
    }

    #[test]
    fn preserves_process_identity_when_filesystem_telemetry_saturates_queue() {
        let q = EventQueue::new(2, Arc::new(Metrics::default()));
        q.push(QueuedEvent::Process(crate::process_exec(
            42,
            1,
            "/home/user/Game.AppImage",
        )));
        for path in ["/home/user/a", "/home/user/b", "/home/user/c"] {
            q.push(QueuedEvent::Filesystem(FilesystemEvent {
                kind: FsEventKind::Write,
                pid: 42,
                path: PathBuf::from(path),
                dest_path: None,
                at: Utc::now(),
                sampled: false,
            }));
        }
        let events = q.drain();
        assert!(events.iter().any(is_process_event));
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn coalesces_repeated_writes() {
        let q = EventQueue::new(16, Arc::new(Metrics::default()));
        for _ in 0..5 {
            q.push(QueuedEvent::Filesystem(FilesystemEvent {
                kind: FsEventKind::Write,
                pid: 1,
                path: PathBuf::from("/tmp/save.dat"),
                dest_path: None,
                at: Utc::now(),
                sampled: false,
            }));
        }
        assert_eq!(q.drain().len(), 1);
    }

    #[test]
    fn does_not_coalesce_apps_sharing_a_namespace_path_or_across_pid_reuse() {
        let q = EventQueue::new(16, Arc::new(Metrics::default()));
        for pid in [41, 42] {
            q.push(QueuedEvent::Filesystem(crate::fs_event(
                FsEventKind::Write,
                pid,
                "/home/runtime/saves/current",
            )));
        }
        q.push(QueuedEvent::Process(crate::process_exec(
            41,
            1,
            "/home/runtime/game",
        )));
        q.push(QueuedEvent::Filesystem(crate::fs_event(
            FsEventKind::Write,
            41,
            "/home/runtime/saves/current",
        )));
        assert_eq!(q.drain().len(), 4);
    }

    #[test]
    fn rename_and_delete_separate_write_coalescing_runs() {
        let q = EventQueue::new(16, Arc::new(Metrics::default()));
        for kind in [
            FsEventKind::Write,
            FsEventKind::Unlink,
            FsEventKind::Write,
            FsEventKind::Rename,
            FsEventKind::Write,
        ] {
            q.push(QueuedEvent::Filesystem(crate::fs_event(
                kind,
                41,
                "/home/runtime/saves/current",
            )));
        }
        assert_eq!(q.drain().len(), 5);
    }

    #[tokio::test]
    async fn push_wakes_an_event_consumer() {
        let q = EventQueue::new(16, Arc::new(Metrics::default()));
        let notified = q.notified();
        q.push(QueuedEvent::Filesystem(FilesystemEvent {
            kind: FsEventKind::Write,
            pid: 1,
            path: PathBuf::from("/tmp/save.dat"),
            dest_path: None,
            at: Utc::now(),
            sampled: false,
        }));

        tokio::time::timeout(std::time::Duration::from_millis(100), notified)
            .await
            .expect("queue push should wake a waiting consumer");
    }

    #[test]
    fn bounded_fifo_prefixes_do_not_drop_events_or_report_loss() {
        let q = EventQueue::new(2048, Arc::new(Metrics::default()));
        for index in 0..1025 {
            q.push(QueuedEvent::Filesystem(crate::fs_event(
                FsEventKind::Write,
                41,
                format!("/home/user/save-{index}"),
            )));
        }
        let mut seen = Vec::new();
        for expected in [512, 512, 1] {
            let batch = q.drain_prefix(512);
            assert_eq!(batch.len(), expected);
            for event in batch {
                let QueuedEvent::Filesystem(event) = event else {
                    panic!("expected filesystem fact");
                };
                seen.push(event.path);
            }
        }
        assert_eq!(
            seen,
            (0..1025)
                .map(|index| PathBuf::from(format!("/home/user/save-{index}")))
                .collect::<Vec<_>>()
        );
        assert!(!q.has_pending());
        assert_eq!(q.dropped(), 0);
        assert!(q.loss_snapshot().is_none());
    }

    #[test]
    fn prefix_removal_does_not_reuse_stale_write_offsets() {
        let q = EventQueue::new(16, Arc::new(Metrics::default()));
        for path in ["/home/user/one", "/home/user/two"] {
            q.push(QueuedEvent::Filesystem(crate::fs_event(
                FsEventKind::Write,
                41,
                path,
            )));
        }
        assert_eq!(q.drain_prefix(1).len(), 1);
        q.push(QueuedEvent::Filesystem(crate::fs_event(
            FsEventKind::Write,
            41,
            "/home/user/two",
        )));
        assert_eq!(q.drain().len(), 2);
        assert!(q.loss_snapshot().is_none());
    }

    #[test]
    fn loss_acknowledgement_cannot_erase_a_newer_overflow() {
        let q = EventQueue::new(1, Arc::new(Metrics::default()));
        q.report_loss(1);
        let first = q.loss_snapshot().unwrap();
        q.report_loss(2);
        assert!(!q.acknowledge_loss(first));
        assert_eq!(q.loss_snapshot().unwrap().dropped, 3);
        assert!(q.acknowledge_loss(q.loss_snapshot().unwrap()));
        assert!(q.loss_snapshot().is_none());
    }

    #[test]
    fn real_capacity_loss_stays_pending_across_prefix_and_full_drains() {
        let q = EventQueue::new(2, Arc::new(Metrics::default()));
        for pid in 1..=3 {
            q.push(QueuedEvent::Filesystem(crate::fs_event(
                FsEventKind::Write,
                pid,
                format!("/home/user/save-{pid}"),
            )));
        }
        assert_eq!(q.dropped(), 1);
        assert!(!q.acknowledge_loss(q.loss_snapshot().unwrap()));
        assert_eq!(q.drain_prefix(1).len(), 1);
        assert!(q.loss_snapshot().is_some());
        assert_eq!(q.drain().len(), 1);
        assert!(q.has_pending(), "unacknowledged loss is still pending work");
        assert!(q.acknowledge_loss(q.loss_snapshot().unwrap()));
        assert!(!q.has_pending());
    }

    #[test]
    fn restoring_a_failed_prefix_preserves_it_and_explicitly_reports_capacity_loss() {
        let q = EventQueue::new(3, Arc::new(Metrics::default()));
        for pid in 1..=3 {
            q.push(QueuedEvent::FilesystemGap { pid });
        }
        let prefix = q.drain_prefix(2);
        q.push(QueuedEvent::FilesystemGap { pid: 4 });
        q.push(QueuedEvent::FilesystemGap { pid: 5 });
        q.restore_front(prefix);
        let pids = q
            .drain()
            .into_iter()
            .map(|event| match event {
                QueuedEvent::FilesystemGap { pid } => pid,
                _ => panic!("expected a gap"),
            })
            .collect::<Vec<_>>();
        assert_eq!(pids, [1, 2, 3]);
        assert_eq!(q.dropped(), 2);
        assert!(q.loss_snapshot().is_some());
    }

    #[test]
    fn restored_prefix_keeps_process_and_rename_boundaries_between_write_runs() {
        let q = EventQueue::new(16, Arc::new(Metrics::default()));
        let write =
            || QueuedEvent::Filesystem(crate::fs_event(FsEventKind::Write, 41, "/home/user/save"));
        q.push(write());
        q.push(QueuedEvent::Process(crate::process_exec(
            41,
            1,
            "/opt/new-app",
        )));
        q.push(write());
        let detached = q.drain_prefix(3);
        q.push(write());
        let mut rename = crate::fs_event(FsEventKind::Rename, 41, "/home/user/save");
        rename.dest_path = Some("/home/user/renamed".into());
        q.push(QueuedEvent::Filesystem(rename));
        q.push(write());
        q.restore_front(detached);
        q.push(write());
        let kinds = q
            .drain()
            .into_iter()
            .map(|event| match event {
                QueuedEvent::Process(_) => "process",
                QueuedEvent::Filesystem(event) if event.kind == FsEventKind::Rename => "rename",
                QueuedEvent::Filesystem(_) => "write",
                _ => unreachable!(),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            ["write", "process", "write", "write", "rename", "write", "write"]
        );
        assert_eq!(q.dropped(), 0);
    }

    #[tokio::test]
    async fn bounded_suffix_is_ready_without_another_producer_notification() {
        let q = EventQueue::new(16, Arc::new(Metrics::default()));
        for pid in 1..=3 {
            q.push(QueuedEvent::FilesystemGap { pid });
        }
        q.notified().await;
        assert_eq!(q.drain_prefix(1).len(), 1);
        tokio::time::timeout(std::time::Duration::from_millis(100), q.notified())
            .await
            .expect("a retained FIFO suffix must be runnable");
        assert_eq!(q.drain().len(), 2);
    }
}
