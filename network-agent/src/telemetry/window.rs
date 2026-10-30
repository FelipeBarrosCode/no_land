use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use super::sample::{MeasurementSample, StoredSample};

pub const MAX_WINDOW_AGE: Duration = Duration::from_secs(5 * 60);
pub const MAX_WINDOW_SAMPLES: usize = 1_500;

#[derive(Debug, Default)]
pub struct TelemetryWindow {
    samples: VecDeque<StoredSample>,
    continuous_since: Option<Instant>,
    last_inserted: Option<Instant>,
    batch_id: Option<uuid::Uuid>,
    recent_batches: VecDeque<uuid::Uuid>,
}

impl TelemetryWindow {
    pub fn push(&mut self, sample: MeasurementSample, now: Instant) {
        self.prune(now);
        if self
            .last_inserted
            .is_none_or(|last| now.saturating_duration_since(last) > Duration::from_secs(3))
        {
            self.continuous_since = Some(now);
        }
        self.last_inserted = Some(now);
        self.samples.push_back(StoredSample::new(sample, now));
        while self.samples.len() > MAX_WINDOW_SAMPLES {
            self.samples.pop_front();
        }
    }

    pub fn extend(&mut self, samples: Vec<MeasurementSample>, now: Instant) {
        for sample in samples {
            self.push(sample, now);
        }
    }

    pub fn prune(&mut self, now: Instant) {
        while self.samples.front().is_some_and(|sample| {
            now.checked_duration_since(sample.inserted_at)
                .is_some_and(|age| age > MAX_WINDOW_AGE)
        }) {
            self.samples.pop_front();
        }
    }

    pub fn record_batch(&mut self, batch_id: Option<uuid::Uuid>) -> bool {
        if let Some(id) = batch_id {
            if self.recent_batches.contains(&id) {
                return false;
            }
            if self.recent_batches.len() >= 128 {
                self.recent_batches.pop_front();
            }
            self.recent_batches.push_back(id);
        }
        self.batch_id = batch_id;
        true
    }

    pub fn feedback_evidence(
        &self,
        now: Instant,
    ) -> Option<noland_network_contracts::feedback::MeasurementEvidence> {
        let received: Vec<_> = self
            .iter_since(now, Duration::from_secs(60))
            .filter(|sample| {
                !sample.lost
                    && sample
                        .rtt_ms
                        .is_some_and(|rtt| rtt.is_finite() && rtt >= 0.0)
            })
            .collect();
        Some(noland_network_contracts::feedback::MeasurementEvidence {
            batch_id: self.batch_id?,
            received_sequence: received.iter().map(|sample| sample.sequence).max(),
            received_rtt_count: received.len() as u64,
            continuous_window_ms: self
                .continuous_since
                .map(|start| now.saturating_duration_since(start).as_millis() as u64)
                .unwrap_or(0),
            latest_received_age_ms: received.last().map(|sample| {
                now.saturating_duration_since(sample.inserted_at)
                    .as_millis() as u64
            }),
            metric_version: noland_network_contracts::feedback::METRIC_VERSION.into(),
        })
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn first_sample_at(&self) -> Option<Instant> {
        self.samples.front().map(|sample| sample.inserted_at)
    }

    pub fn iter_since(
        &self,
        now: Instant,
        duration: Duration,
    ) -> impl Iterator<Item = &StoredSample> {
        self.samples.iter().filter(move |sample| {
            now.checked_duration_since(sample.inserted_at)
                .is_none_or(|age| age <= duration)
        })
    }
}

#[cfg(test)]
mod feedback_evidence_tests {
    use super::*;
    #[test]
    fn evidence_requires_processed_batch_and_counts_received_not_sent() {
        let base = Instant::now();
        let mut window = TelemetryWindow::default();
        for sequence in 0..=60 {
            window.push(
                MeasurementSample {
                    sequence,
                    rtt_ms: None,
                    lost: true,
                },
                base + Duration::from_secs(sequence),
            );
        }
        assert!(window
            .feedback_evidence(base + Duration::from_secs(60))
            .is_none());
        let batch = uuid::Uuid::new_v4();
        assert!(window.record_batch(Some(batch)));
        let evidence = window
            .feedback_evidence(base + Duration::from_secs(60))
            .unwrap();
        assert_eq!(evidence.received_rtt_count, 0);
        assert!(evidence.received_sequence.is_none());
        assert!(evidence.latest_received_age_ms.is_none());
        assert_eq!(evidence.continuous_window_ms, 60_000);
        assert!(!window.record_batch(Some(batch)));
    }
    #[test]
    fn fresh_received_watermark_and_full_window_reset_after_a_gap() {
        let base = Instant::now();
        let mut window = TelemetryWindow::default();
        window.record_batch(Some(uuid::Uuid::new_v4()));
        for sequence in 0..=60 {
            window.push(
                MeasurementSample {
                    sequence,
                    rtt_ms: Some(18.0),
                    lost: false,
                },
                base + Duration::from_secs(sequence),
            );
        }
        let evidence = window
            .feedback_evidence(base + Duration::from_secs(60))
            .unwrap();
        assert_eq!(evidence.received_rtt_count, 61);
        assert_eq!(evidence.received_sequence, Some(60));
        assert_eq!(evidence.latest_received_age_ms, Some(0));
        assert_eq!(evidence.continuous_window_ms, 60_000);
        window.push(
            MeasurementSample {
                sequence: 61,
                rtt_ms: Some(18.0),
                lost: false,
            },
            base + Duration::from_secs(65),
        );
        let evidence = window
            .feedback_evidence(base + Duration::from_secs(65))
            .unwrap();
        assert_eq!(evidence.continuous_window_ms, 0);
        assert_eq!(
            window
                .feedback_evidence(base + Duration::from_secs(70))
                .unwrap()
                .latest_received_age_ms,
            Some(5_000)
        );
    }
}
