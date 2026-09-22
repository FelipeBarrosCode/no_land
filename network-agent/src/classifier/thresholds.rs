use std::time::Duration;

#[derive(Debug, Clone)]
pub struct ClassifierThresholds {
    pub loss_percent: [f64; 3],
    pub jitter_ms: [f64; 3],
    pub p95_median_spread_ms: [f64; 3],
    pub spike_percent: [f64; 3],
    pub latency_median_ms: [f64; 3],
    pub loss_burst_reason: usize,
    pub warmup_duration: Duration,
    pub warmup_samples: usize,
}

impl Default for ClassifierThresholds {
    fn default() -> Self {
        Self {
            loss_percent: [0.1, 0.5, 2.0],
            jitter_ms: [3.0, 7.0, 15.0],
            p95_median_spread_ms: [10.0, 20.0, 40.0],
            spike_percent: [1.0, 3.0, 10.0],
            latency_median_ms: [30.0, 60.0, 100.0],
            loss_burst_reason: 3,
            warmup_duration: Duration::from_secs(5),
            warmup_samples: 25,
        }
    }
}
