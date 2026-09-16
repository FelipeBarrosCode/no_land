use std::time::Instant;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeasurementSample {
    pub sequence: u64,
    #[serde(default)]
    pub rtt_ms: Option<f64>,
    pub lost: bool,
}

impl MeasurementSample {
    pub fn is_valid(&self) -> bool {
        match (self.lost, self.rtt_ms) {
            (true, None) => true,
            (true, Some(_)) => false,
            (false, Some(rtt)) => rtt.is_finite() && rtt >= 0.0,
            (false, None) => false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct StoredSample {
    pub sequence: u64,
    pub rtt_ms: Option<f64>,
    pub lost: bool,
    pub inserted_at: Instant,
}

impl StoredSample {
    pub fn new(sample: MeasurementSample, inserted_at: Instant) -> Self {
        Self {
            sequence: sample.sequence,
            rtt_ms: sample.rtt_ms,
            lost: sample.lost,
            inserted_at,
        }
    }
}
