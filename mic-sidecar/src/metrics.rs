use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Default)]
pub struct Metrics {
    captured_samples: AtomicU64,
    consumed_samples: AtomicU64,
    dropped_stale_samples: AtomicU64,
    overruns: AtomicU64,
    underruns: AtomicU64,
    silence_samples: AtomicU64,
    buffers_pushed: AtomicU64,
    capture_restarts: AtomicU64,
    capture_errors: AtomicU64,
    pipeline_errors: AtomicU64,
    ring_depth_samples: AtomicU64,
    appsrc_queue_ns: AtomicU64,
    opus_packets_sent: AtomicU64,
    bytes_sent: AtomicU64,
    current_rtp_sequence: AtomicU64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MetricsSnapshot {
    pub captured_samples: u64,
    pub consumed_samples: u64,
    pub dropped_stale_samples: u64,
    pub overruns: u64,
    pub underruns: u64,
    pub silence_samples: u64,
    pub buffers_pushed: u64,
    pub capture_restarts: u64,
    pub capture_errors: u64,
    pub pipeline_errors: u64,
    pub ring_depth_samples: u64,
    pub appsrc_queue_ms: u64,
    pub opus_packets_sent: u64,
    pub bytes_sent: u64,
    pub current_rtp_sequence: Option<u16>,
    pub sampled_at_unix_ms: u64,
}

impl Metrics {
    pub fn record_capture(&self, samples: u64, dropped: u64, overrun: bool) {
        self.captured_samples.fetch_add(samples, Ordering::Relaxed);
        self.dropped_stale_samples
            .fetch_add(dropped, Ordering::Relaxed);
        if overrun {
            self.overruns.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn record_output(&self, consumed: u64, silence: u64, underrun: bool) {
        self.consumed_samples.fetch_add(consumed, Ordering::Relaxed);
        self.silence_samples.fetch_add(silence, Ordering::Relaxed);
        self.buffers_pushed.fetch_add(1, Ordering::Relaxed);
        if underrun {
            self.underruns.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn capture_restart(&self) {
        self.capture_restarts.fetch_add(1, Ordering::Relaxed);
    }

    pub fn capture_error(&self) {
        self.capture_errors.fetch_add(1, Ordering::Relaxed);
    }

    pub fn pipeline_error(&self) {
        self.pipeline_errors.fetch_add(1, Ordering::Relaxed);
    }

    pub fn set_ring_depth(&self, samples: usize) {
        self.ring_depth_samples
            .store(samples as u64, Ordering::Relaxed);
    }

    pub fn set_appsrc_queue_ns(&self, nanoseconds: u64) {
        self.appsrc_queue_ns.store(nanoseconds, Ordering::Relaxed);
    }

    pub fn record_rtp_packet(&self, bytes: usize, sequence: u16) {
        self.opus_packets_sent.fetch_add(1, Ordering::Relaxed);
        self.bytes_sent.fetch_add(bytes as u64, Ordering::Relaxed);
        self.current_rtp_sequence
            .store(u64::from(sequence) + 1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            captured_samples: self.captured_samples.load(Ordering::Relaxed),
            consumed_samples: self.consumed_samples.load(Ordering::Relaxed),
            dropped_stale_samples: self.dropped_stale_samples.load(Ordering::Relaxed),
            overruns: self.overruns.load(Ordering::Relaxed),
            underruns: self.underruns.load(Ordering::Relaxed),
            silence_samples: self.silence_samples.load(Ordering::Relaxed),
            buffers_pushed: self.buffers_pushed.load(Ordering::Relaxed),
            capture_restarts: self.capture_restarts.load(Ordering::Relaxed),
            capture_errors: self.capture_errors.load(Ordering::Relaxed),
            pipeline_errors: self.pipeline_errors.load(Ordering::Relaxed),
            ring_depth_samples: self.ring_depth_samples.load(Ordering::Relaxed),
            appsrc_queue_ms: self.appsrc_queue_ns.load(Ordering::Relaxed) / 1_000_000,
            opus_packets_sent: self.opus_packets_sent.load(Ordering::Relaxed),
            bytes_sent: self.bytes_sent.load(Ordering::Relaxed),
            current_rtp_sequence: self
                .current_rtp_sequence
                .load(Ordering::Relaxed)
                .checked_sub(1)
                .map(|value| value as u16),
            sampled_at_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_contains_accumulated_metrics() {
        let metrics = Metrics::default();
        metrics.record_capture(480, 12, true);
        metrics.record_output(470, 10, true);
        metrics.capture_restart();
        metrics.capture_error();
        metrics.pipeline_error();
        metrics.set_ring_depth(100);
        metrics.set_appsrc_queue_ns(12_000_000);
        metrics.record_rtp_packet(120, 77);
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.captured_samples, 480);
        assert_eq!(snapshot.consumed_samples, 470);
        assert_eq!(snapshot.dropped_stale_samples, 12);
        assert_eq!(snapshot.overruns, 1);
        assert_eq!(snapshot.underruns, 1);
        assert_eq!(snapshot.silence_samples, 10);
        assert_eq!(snapshot.buffers_pushed, 1);
        assert_eq!(snapshot.capture_restarts, 1);
        assert_eq!(snapshot.capture_errors, 1);
        assert_eq!(snapshot.pipeline_errors, 1);
        assert_eq!(snapshot.ring_depth_samples, 100);
        assert_eq!(snapshot.appsrc_queue_ms, 12);
        assert_eq!(snapshot.opus_packets_sent, 1);
        assert_eq!(snapshot.bytes_sent, 120);
        assert_eq!(snapshot.current_rtp_sequence, Some(77));
    }
}
