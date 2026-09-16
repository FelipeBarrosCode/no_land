use std::time::{Duration, Instant};

use serde::Serialize;

use super::{sample::StoredSample, window::TelemetryWindow};

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CurrentMetrics {
    pub rtt_ms: Option<f64>,
    pub jitter_ms: f64,
    pub lost: Option<bool>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WindowMetrics {
    pub sent: usize,
    pub received: usize,
    pub lost: usize,
    pub loss_percent: f64,
    pub min_rtt_ms: Option<f64>,
    pub avg_rtt_ms: Option<f64>,
    pub median_rtt_ms: Option<f64>,
    pub max_rtt_ms: Option<f64>,
    pub p95_rtt_ms: Option<f64>,
    pub p99_rtt_ms: Option<f64>,
    pub jitter_ms: f64,
    pub spikes: usize,
    pub spike_percent: f64,
    pub longest_loss_burst: usize,
}

impl WindowMetrics {
    pub fn from_samples<'a>(samples: impl IntoIterator<Item = &'a StoredSample>) -> Self {
        let samples: Vec<&StoredSample> = samples.into_iter().collect();
        let sent = samples.len();
        let lost = samples.iter().filter(|sample| sample.lost).count();
        let received = sent.saturating_sub(lost);
        let loss_percent = percentage(lost, sent);

        let mut rtts: Vec<f64> = samples.iter().filter_map(|sample| sample.rtt_ms).collect();
        rtts.sort_by(f64::total_cmp);

        let jitter_ms = ewma_jitter(samples.iter().copied());
        let median_rtt_ms = median(&rtts);
        let p95_rtt_ms = percentile(&rtts, 0.95);
        let spike_threshold = median_rtt_ms.map(|median| median + 15.0_f64.max(jitter_ms * 3.0));
        let spikes = spike_threshold
            .map(|threshold| rtts.iter().filter(|rtt| **rtt > threshold).count())
            .unwrap_or_default();

        Self {
            sent,
            received,
            lost,
            loss_percent,
            min_rtt_ms: rtts.first().copied(),
            avg_rtt_ms: (!rtts.is_empty()).then(|| rtts.iter().sum::<f64>() / rtts.len() as f64),
            median_rtt_ms,
            max_rtt_ms: rtts.last().copied(),
            p95_rtt_ms,
            p99_rtt_ms: percentile(&rtts, 0.99),
            jitter_ms,
            spikes,
            spike_percent: percentage(spikes, received),
            longest_loss_burst: longest_loss_burst(&samples),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MetricsSnapshot {
    pub current: CurrentMetrics,
    pub last_60_seconds: WindowMetrics,
}

pub fn snapshot(window: &TelemetryWindow, now: Instant) -> MetricsSnapshot {
    let samples: Vec<&StoredSample> = window.iter_since(now, Duration::from_secs(60)).collect();
    let last_60_seconds = WindowMetrics::from_samples(samples.iter().copied());
    let latest = samples.last().copied();
    let current_rtt = samples.iter().rev().find_map(|sample| sample.rtt_ms);

    MetricsSnapshot {
        current: CurrentMetrics {
            rtt_ms: current_rtt,
            jitter_ms: last_60_seconds.jitter_ms,
            lost: latest.map(|sample| sample.lost),
        },
        last_60_seconds,
    }
}

pub fn metrics_for_duration(
    window: &TelemetryWindow,
    now: Instant,
    duration: Duration,
) -> WindowMetrics {
    WindowMetrics::from_samples(window.iter_since(now, duration))
}

fn percentage(part: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        part as f64 * 100.0 / total as f64
    }
}

fn median(sorted: &[f64]) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let middle = sorted.len() / 2;
    if sorted.len() & 1 == 0 {
        Some((sorted[middle - 1] + sorted[middle]) / 2.0)
    } else {
        Some(sorted[middle])
    }
}

fn percentile(sorted: &[f64], percentile: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (percentile * sorted.len() as f64).ceil() as usize;
    Some(sorted[rank.saturating_sub(1).min(sorted.len() - 1)])
}

fn ewma_jitter<'a>(samples: impl IntoIterator<Item = &'a StoredSample>) -> f64 {
    let mut previous = None;
    let mut jitter = 0.0;
    for rtt in samples.into_iter().filter_map(|sample| sample.rtt_ms) {
        if let Some(previous_rtt) = previous {
            let difference: f64 = rtt - previous_rtt;
            jitter += (difference.abs() - jitter) / 16.0;
        }
        previous = Some(rtt);
    }
    jitter
}

fn longest_loss_burst(samples: &[&StoredSample]) -> usize {
    let mut longest = 0;
    let mut current = 0;
    for sample in samples {
        if sample.lost {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::sample::MeasurementSample;

    #[test]
    fn constant_rtt_metrics_are_exact() {
        let base = Instant::now();
        let mut window = TelemetryWindow::default();
        for sequence in 0..30 {
            window.push(
                MeasurementSample {
                    sequence,
                    rtt_ms: Some(18.0),
                    lost: false,
                },
                base,
            );
        }

        let metrics = snapshot(&window, base).last_60_seconds;
        assert_eq!(metrics.sent, 30);
        assert_eq!(metrics.received, 30);
        assert_eq!(metrics.loss_percent, 0.0);
        assert_eq!(metrics.min_rtt_ms, Some(18.0));
        assert_eq!(metrics.avg_rtt_ms, Some(18.0));
        assert_eq!(metrics.median_rtt_ms, Some(18.0));
        assert_eq!(metrics.p95_rtt_ms, Some(18.0));
        assert_eq!(metrics.p99_rtt_ms, Some(18.0));
        assert_eq!(metrics.jitter_ms, 0.0);
        assert_eq!(metrics.spikes, 0);
    }
}
