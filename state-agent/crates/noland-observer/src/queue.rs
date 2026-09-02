use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use noland_state_core::metrics::Metrics;
use noland_state_core::{
    EbpfFilesystemFact, EbpfProcessFact, FilesystemEvent, FsEventKind, ProcessEvent,
};
use parking_lot::Mutex;

#[derive(Debug, Clone)]
pub enum QueuedEvent {
    Process(ProcessEvent),
    Filesystem(FilesystemEvent),
    EbpfProcess(EbpfProcessFact),
    EbpfFilesystem(EbpfFilesystemFact),
}

pub struct EventQueue {
    cap: usize,
    inner: Mutex<Inner>,
    metrics: Arc<Metrics>,
}

struct Inner {
    events: VecDeque<QueuedEvent>,
    last_write: HashMap<String, usize>,
    dropped: u64,
    loss_detected: bool,
}

impl EventQueue {
    pub fn new(cap: usize, metrics: Arc<Metrics>) -> Self {
        Self {
            cap,
            metrics,
            inner: Mutex::new(Inner {
                events: VecDeque::with_capacity(cap.min(4096)),
                last_write: HashMap::new(),
                dropped: 0,
                loss_detected: false,
            }),
        }
    }

    pub fn push(&self, event: QueuedEvent) {
        let mut inner = self.inner.lock();
        if let Some((kind, path, at)) = filesystem_identity(&event) {
            if matches!(kind, FsEventKind::Write | FsEventKind::Truncate) {
                let key = path.to_string_lossy().into_owned();
                if let Some(idx) = inner.last_write.get(&key).copied() {
                    if idx < inner.events.len() {
                        match inner.events.get_mut(idx) {
                            Some(QueuedEvent::Filesystem(existing)) => existing.at = at,
                            Some(QueuedEvent::EbpfFilesystem(existing)) => existing.at = at,
                            _ => {}
                        }
                        Metrics::inc(&self.metrics.events_coalesced_total);
                        return;
                    }
                }
            }
        }

        if inner.events.len() >= self.cap {
            // Drop lowest-value events first: read-only filesystem telemetry.
            if let Some(pos) = inner.events.iter().position(|event| {
                filesystem_identity(event)
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
                .position(|queued| filesystem_identity(queued).is_some())
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
                return;
            }
        }
        let write_key = filesystem_identity(&event).and_then(|(kind, path, _)| {
            matches!(kind, FsEventKind::Write | FsEventKind::Truncate)
                .then(|| path.to_string_lossy().into_owned())
        });
        let idx = inner.events.len();
        if let Some(key) = write_key {
            inner.last_write.insert(key, idx);
        }
        inner.events.push_back(event);
    }

    pub fn drain(&self) -> Vec<QueuedEvent> {
        let mut inner = self.inner.lock();
        inner.last_write.clear();
        inner.events.drain(..).collect()
    }

    pub fn report_loss(&self, count: u64) {
        if count == 0 {
            return;
        }
        let mut inner = self.inner.lock();
        inner.dropped = inner.dropped.saturating_add(count);
        inner.loss_detected = true;
        Metrics::add(&self.metrics.filesystem_events_dropped_total, count);
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
        QueuedEvent::Process(_) | QueuedEvent::EbpfProcess(_) => None,
    }
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
}
