pub mod thresholds;

use std::time::{Duration, Instant};

use serde::Serialize;

use crate::telemetry::{
    metrics::{metrics_for_duration, WindowMetrics},
    window::TelemetryWindow,
};
use thresholds::ClassifierThresholds;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Quality {
    WarmingUp,
    Great,
    Good,
    Poor,
    Bad,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReasonCode {
    HighLatency,
    HighJitter,
    PacketLoss,
    LossBurst,
    LatencySpikes,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Classification {
    pub status: Quality,
    pub stability: Quality,
    pub latency: Quality,
    pub reasons: Vec<ReasonCode>,
}

pub fn classify(
    window: &TelemetryWindow,
    now: Instant,
    thresholds: &ClassifierThresholds,
) -> Classification {
    let old_enough = window
        .first_sample_at()
        .is_some_and(|first| now.saturating_duration_since(first) >= thresholds.warmup_duration);
    if !old_enough || window.len() < thresholds.warmup_samples {
        return Classification {
            status: Quality::WarmingUp,
            stability: Quality::WarmingUp,
            latency: Quality::WarmingUp,
            reasons: Vec::new(),
        };
    }

    let metrics = metrics_for_duration(window, now, Duration::from_secs(30));
    classify_metrics(&metrics, thresholds)
}

fn classify_metrics(metrics: &WindowMetrics, thresholds: &ClassifierThresholds) -> Classification {
    let median = metrics.median_rtt_ms.unwrap_or_default();
    let p95_spread = metrics.p95_rtt_ms.unwrap_or(median) - median;
    let stability = [
        bounded_quality(metrics.loss_percent, thresholds.loss_percent),
        bounded_quality(metrics.jitter_ms, thresholds.jitter_ms),
        bounded_quality(p95_spread, thresholds.p95_median_spread_ms),
        bounded_quality(metrics.spike_percent, thresholds.spike_percent),
    ]
    .into_iter()
    .max_by_key(|quality| quality_rank(*quality))
    .unwrap_or(Quality::Great);

    let latency = strict_quality(median, thresholds.latency_median_ms);
    let status = if quality_rank(stability) >= quality_rank(latency) {
        stability
    } else {
        latency
    };

    let mut reasons = Vec::new();
    if latency != Quality::Great {
        reasons.push(ReasonCode::HighLatency);
    }
    if metrics.jitter_ms > thresholds.jitter_ms[0] {
        reasons.push(ReasonCode::HighJitter);
    }
    if metrics.loss_percent > thresholds.loss_percent[0] {
        reasons.push(ReasonCode::PacketLoss);
    }
    if metrics.longest_loss_burst >= thresholds.loss_burst_reason {
        reasons.push(ReasonCode::LossBurst);
    }
    if metrics.spike_percent > thresholds.spike_percent[0] {
        reasons.push(ReasonCode::LatencySpikes);
    }

    Classification {
        status,
        stability,
        latency,
        reasons,
    }
}

fn bounded_quality(value: f64, thresholds: [f64; 3]) -> Quality {
    if value <= thresholds[0] {
        Quality::Great
    } else if value <= thresholds[1] {
        Quality::Good
    } else if value <= thresholds[2] {
        Quality::Poor
    } else {
        Quality::Bad
    }
}

fn strict_quality(value: f64, thresholds: [f64; 3]) -> Quality {
    if value < thresholds[0] {
        Quality::Great
    } else if value < thresholds[1] {
        Quality::Good
    } else if value < thresholds[2] {
        Quality::Poor
    } else {
        Quality::Bad
    }
}

fn quality_rank(quality: Quality) -> u8 {
    match quality {
        Quality::WarmingUp => 0,
        Quality::Great => 1,
        Quality::Good => 2,
        Quality::Poor => 3,
        Quality::Bad => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::sample::MeasurementSample;

    fn window_with(values: &[Option<f64>]) -> (TelemetryWindow, Instant) {
        let base = Instant::now();
        let mut window = TelemetryWindow::default();
        for (sequence, value) in values.iter().enumerate() {
            window.push(
                MeasurementSample {
                    sequence: sequence as u64,
                    rtt_ms: *value,
                    lost: value.is_none(),
                },
                base,
            );
        }
        (window, base + Duration::from_secs(6))
    }

    #[test]
    fn constant_18ms_is_great() {
        let (window, now) = window_with(&vec![Some(18.0); 30]);
        let result = classify(&window, now, &ClassifierThresholds::default());
        assert_eq!(result.status, Quality::Great);
        assert_eq!(result.stability, Quality::Great);
        assert_eq!(result.latency, Quality::Great);
    }

    #[test]
    fn stable_60ms_has_great_stability_and_poor_latency() {
        let (window, now) = window_with(&vec![Some(60.0); 30]);
        let result = classify(&window, now, &ClassifierThresholds::default());
        assert_eq!(result.stability, Quality::Great);
        assert_eq!(result.latency, Quality::Poor);
        assert_eq!(result.status, Quality::Poor);
    }

    #[test]
    fn jittery_values_are_degraded() {
        let values: Vec<Option<f64>> = (0..40)
            .map(|index| Some(if index % 2 == 0 { 20.0 } else { 80.0 }))
            .collect();
        let (window, now) = window_with(&values);
        let result = classify(&window, now, &ClassifierThresholds::default());
        assert_ne!(result.stability, Quality::Great);
        assert!(result.reasons.contains(&ReasonCode::HighJitter));
    }

    #[test]
    fn three_percent_loss_is_bad() {
        let mut values = vec![Some(18.0); 100];
        values[10] = None;
        values[40] = None;
        values[80] = None;
        let (window, now) = window_with(&values);
        let result = classify(&window, now, &ClassifierThresholds::default());
        assert_eq!(result.stability, Quality::Bad);
        assert_eq!(result.status, Quality::Bad);
        assert!(result.reasons.contains(&ReasonCode::PacketLoss));
    }
}
