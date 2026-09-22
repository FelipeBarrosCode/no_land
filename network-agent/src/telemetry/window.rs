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
}

impl TelemetryWindow {
    pub fn push(&mut self, sample: MeasurementSample, now: Instant) {
        self.prune(now);
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
