use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use noland_state_core::metrics::Metrics;
use noland_state_core::{FilesystemEvent, FsEventKind, ProcessEvent};
use parking_lot::Mutex;

#[derive(Debug, Clone)]
pub enum QueuedEvent {
    Process(ProcessEvent),
    Filesystem(FilesystemEvent),
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
        if let QueuedEvent::Filesystem(fs) = &event {
            if matches!(fs.kind, FsEventKind::Write | FsEventKind::Truncate) {
                let key = fs.path.to_string_lossy().into_owned();
                if let Some(idx) = inner.last_write.get(&key).copied() {
                    if idx < inner.events.len() {
                        if let Some(QueuedEvent::Filesystem(existing)) = inner.events.get_mut(idx) {
                            existing.at = fs.at;
                            Metrics::inc(&self.metrics.events_coalesced_total);
                            return;
                        }
                    }
                }
            }
        }

        if inner.events.len() >= self.cap {
            // Drop lowest-value events first: read-only filesystem telemetry.
            if let Some(pos) = inner.events.iter().position(|e| {
                matches!(e, QueuedEvent::Filesystem(fs) if fs.kind.is_read() && !fs.kind.is_mutation())
            }) {
                inner.events.remove(pos);
                inner.dropped += 1;
                inner.loss_detected = true;
                Metrics::inc(&self.metrics.filesystem_events_dropped_total);
            } else if inner.events.pop_front().is_some() {
                inner.dropped += 1;
                inner.loss_detected = true;
                Metrics::inc(&self.metrics.filesystem_events_dropped_total);
            }
        }
        let write_key = if let QueuedEvent::Filesystem(fs) = &event {
            if matches!(fs.kind, FsEventKind::Write | FsEventKind::Truncate) {
                Some(fs.path.to_string_lossy().into_owned())
            } else {
                None
            }
        } else {
            None
        };
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use noland_state_core::metrics::Metrics;
    use std::path::PathBuf;
    use std::sync::Arc;

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
