//! Privacy-safe feedback metric contract. No packet samples or client identifiers leave this accumulator.
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const METRIC_VERSION: &str = "rtt_jitter_ewma_window60_report1s_v1";
pub const THRESHOLD_VERSION: &str = "noland_jitter_bands_v1";
pub const MAX_BUCKETS: usize = 32;

pub fn provider_id(value: &serde_json::Value) -> Option<String> {
    let number = match value {
        serde_json::Value::String(s) if !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()) => {
            s.parse::<u64>().ok()?
        }
        serde_json::Value::Number(n) => n.as_u64()?,
        _ => return None,
    };
    (number > 0 && number <= i64::MAX as u64).then(|| number.to_string())
}

pub fn valid_provider_id(value: &str) -> bool {
    provider_id(&serde_json::Value::String(value.into())).as_deref() == Some(value)
}

/// Echoed only after the agent has processed this exact measurement batch.
/// Old agents omit this field and are explicitly ineligible for feedback jitter.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MeasurementEvidence {
    pub batch_id: Uuid,
    pub received_sequence: Option<u64>,
    pub received_rtt_count: u64,
    pub continuous_window_ms: u64,
    pub latest_received_age_ms: Option<u64>,
    pub metric_version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteMode {
    Direct,
    Relay,
    Mixed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JitterBucket {
    pub metric_version: String,
    pub route_mode: RouteMode,
    pub completeness: String,
    pub nominal_report_interval_ms: u64,
    pub threshold_version: String,
    pub high_threshold_ms: f64,
    pub severe_threshold_ms: f64,
    pub sample_count: u64,
    pub sum_ms: f64,
    pub min_ms: f64,
    pub max_ms: f64,
    pub high_sample_count: u64,
    pub severe_sample_count: u64,
    pub observed_duration_ms: u64,
    pub first_observed_at: String,
    pub last_observed_at: String,
}

/// All clocks here are monotonic. Wall time is descriptive only.
pub struct JitterObservation<'a> {
    pub evidence: &'a MeasurementEvidence,
    pub jitter_ms: f64,
    pub measured_at: Instant,
    pub received_at: Instant,
    pub observed_at: String,
    pub route: RouteMode,
}

#[derive(Default)]
pub struct JitterAccumulator {
    buckets: Vec<JitterBucket>,
    segment: Option<Segment>,
}

struct Segment {
    id: Uuid,
    started: Instant,
    route: RouteMode,
    route_since: Instant,
    last_batch: Option<Uuid>,
    last_received_sequence: Option<u64>,
    last_slot: Option<u64>,
    last_observation: Option<Instant>,
    last_seen: Option<Instant>,
}

impl JitterAccumulator {
    pub fn restore(buckets: Vec<JitterBucket>) -> Self {
        Self {
            buckets,
            segment: None,
        }
    }

    /// A new streaming segment or reconnect gap must warm a complete window again.
    pub fn start(&mut self, id: Uuid, now: Instant, route: RouteMode) {
        if self
            .segment
            .as_ref()
            .is_some_and(|segment| segment.id == id)
        {
            return;
        }
        self.segment = Some(Segment {
            id,
            started: now,
            route,
            route_since: now,
            last_batch: None,
            last_received_sequence: None,
            last_slot: None,
            last_observation: None,
            last_seen: None,
        });
    }

    pub fn stop(&mut self) {
        self.segment = None;
    }

    pub fn route_changed(&mut self, route: RouteMode, now: Instant) {
        if let Some(segment) = &mut self.segment {
            if segment.route != route {
                segment.route = route;
                segment.route_since = now;
            }
        }
    }
    pub fn buckets(&self) -> &[JitterBucket] {
        &self.buckets
    }

    pub fn observe(&mut self, observation: JitterObservation<'_>) -> bool {
        let Some(segment) = &mut self.segment else {
            return false;
        };
        let e = observation.evidence;
        let now = observation.measured_at;
        // A suspension/measurement gap cannot warm a canonical window or add coverage.
        if segment.last_seen.is_some_and(|previous| {
            now.checked_duration_since(previous)
                .is_none_or(|gap| gap > Duration::from_secs(3))
        }) {
            segment.started = now;
            segment.route_since = now;
            segment.last_observation = None;
            segment.last_slot = None;
        }
        segment.last_seen = Some(now);
        if segment.route != observation.route {
            segment.route = observation.route;
            segment.route_since = now;
        }
        let age = observation.received_at.checked_duration_since(now);
        if !observation.jitter_ms.is_finite()
            || observation.jitter_ms < 0.0
            || e.metric_version != METRIC_VERSION
            || e.received_rtt_count < 2
            || e.latest_received_age_ms.is_none_or(|age| age > 3_000)
            || age.is_none_or(|age| age > Duration::from_secs(3))
            || now
                .checked_duration_since(segment.started)
                .is_none_or(|age| age < Duration::from_secs(60))
            || e.continuous_window_ms < 60_000
            || segment.last_batch == Some(e.batch_id)
            || e.received_sequence.is_none()
            || segment
                .last_received_sequence
                .zip(e.received_sequence)
                .is_some_and(|(old, new)| new <= old)
        {
            return false;
        }
        let slot = now.duration_since(segment.started).as_millis() as u64 / 1_000;
        if segment.last_slot.is_some_and(|previous| slot <= previous) {
            return false;
        }
        let coverage = segment
            .last_observation
            .and_then(|previous| now.checked_duration_since(previous))
            .filter(|elapsed| *elapsed <= Duration::from_secs(3))
            .map(|elapsed| elapsed.as_millis().min(1_000) as u64)
            // This first report also proves a fully measured preceding cadence
            // interval. Never charge the gap between separate warmed segments.
            .unwrap_or(1_000);
        let route = if now
            .checked_duration_since(segment.route_since)
            .unwrap_or_default()
            < Duration::from_secs(60)
        {
            RouteMode::Mixed
        } else {
            observation.route
        };
        let index = match self.buckets.iter().position(|bucket| {
            bucket.route_mode == route
                && bucket.metric_version == METRIC_VERSION
                && bucket.nominal_report_interval_ms == 1_000
                && bucket.completeness == "full_window"
                && bucket.threshold_version == THRESHOLD_VERSION
                && bucket.high_threshold_ms == 7.0
                && bucket.severe_threshold_ms == 15.0
        }) {
            Some(index) => index,
            None if self.buckets.len() < MAX_BUCKETS => {
                self.buckets.push(JitterBucket {
                    metric_version: METRIC_VERSION.into(),
                    route_mode: route,
                    completeness: "full_window".into(),
                    nominal_report_interval_ms: 1_000,
                    threshold_version: THRESHOLD_VERSION.into(),
                    high_threshold_ms: 7.0,
                    severe_threshold_ms: 15.0,
                    sample_count: 0,
                    sum_ms: 0.0,
                    min_ms: observation.jitter_ms,
                    max_ms: observation.jitter_ms,
                    high_sample_count: 0,
                    severe_sample_count: 0,
                    observed_duration_ms: 0,
                    first_observed_at: observation.observed_at.clone(),
                    last_observed_at: observation.observed_at.clone(),
                });
                self.buckets.len() - 1
            }
            None => return false,
        };
        let bucket = &mut self.buckets[index];
        let sum = bucket.sum_ms + observation.jitter_ms;
        if !sum.is_finite() || bucket.sample_count >= 10_000_000 {
            return false;
        }
        bucket.sample_count += 1;
        bucket.sum_ms = sum;
        bucket.min_ms = bucket.min_ms.min(observation.jitter_ms);
        bucket.max_ms = bucket.max_ms.max(observation.jitter_ms);
        bucket.high_sample_count += u64::from(observation.jitter_ms > 7.0);
        bucket.severe_sample_count += u64::from(observation.jitter_ms > 15.0);
        bucket.observed_duration_ms += coverage;
        // Sequence/coverage use monotonic time. Clamp descriptive wall time if the clock moves back.
        if observation.observed_at > bucket.last_observed_at {
            bucket.last_observed_at = observation.observed_at;
        }
        segment.last_batch = Some(e.batch_id);
        segment.last_received_sequence = e.received_sequence;
        segment.last_slot = Some(slot);
        segment.last_observation = Some(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence(sequence: u64) -> MeasurementEvidence {
        MeasurementEvidence {
            batch_id: Uuid::new_v4(),
            received_sequence: Some(sequence),
            received_rtt_count: 10,
            continuous_window_ms: 60_000,
            latest_received_age_ms: Some(0),
            metric_version: METRIC_VERSION.into(),
        }
    }
    fn add(
        a: &mut JitterAccumulator,
        e: &MeasurementEvidence,
        at: Instant,
        value: f64,
        route: RouteMode,
    ) -> bool {
        a.observe(JitterObservation {
            evidence: e,
            jitter_ms: value,
            measured_at: at,
            received_at: at,
            observed_at: "2026-10-26T12:00:00Z".into(),
            route,
        })
    }
    #[test]
    fn deduplicates_real_batches_and_requires_two_received_rtts() {
        let base = Instant::now();
        let mut a = JitterAccumulator::default();
        a.start(Uuid::new_v4(), base, RouteMode::Direct);
        let mut e = evidence(1);
        e.received_rtt_count = 1;
        assert!(!add(
            &mut a,
            &e,
            base + Duration::from_secs(60),
            0.0,
            RouteMode::Direct
        ));
        e.received_rtt_count = 2;
        assert!(add(
            &mut a,
            &e,
            base + Duration::from_secs(60),
            0.0,
            RouteMode::Direct
        ));
        assert!(!add(
            &mut a,
            &e,
            base + Duration::from_secs(61),
            0.0,
            RouteMode::Direct
        ));
        assert_eq!(a.buckets()[0].sample_count, 1);
        assert_eq!(a.buckets()[0].sum_ms, 0.0);
    }
    #[test]
    fn excludes_startup_stale_loss_and_nonfinite_gauges() {
        let base = Instant::now();
        let mut a = JitterAccumulator::default();
        a.start(Uuid::new_v4(), base, RouteMode::Direct);
        let mut e = evidence(1);
        assert!(!add(
            &mut a,
            &e,
            base + Duration::from_secs(59),
            1.0,
            RouteMode::Direct
        ));
        e.latest_received_age_ms = None;
        assert!(!add(
            &mut a,
            &e,
            base + Duration::from_secs(60),
            0.0,
            RouteMode::Direct
        ));
        e.latest_received_age_ms = Some(4_000);
        assert!(!add(
            &mut a,
            &e,
            base + Duration::from_secs(60),
            0.0,
            RouteMode::Direct
        ));
        e.latest_received_age_ms = Some(0);
        assert!(!add(
            &mut a,
            &e,
            base + Duration::from_secs(60),
            f64::NAN,
            RouteMode::Direct
        ));
        a.stop();
        assert!(!add(
            &mut a,
            &e,
            base + Duration::from_secs(61),
            1.0,
            RouteMode::Direct
        ));
    }
    #[test]
    fn mixed_route_warms_out_and_gaps_do_not_invent_coverage() {
        let base = Instant::now();
        let mut a = JitterAccumulator::default();
        a.start(Uuid::new_v4(), base, RouteMode::Direct);
        assert!(add(
            &mut a,
            &evidence(1),
            base + Duration::from_secs(60),
            7.0,
            RouteMode::Direct
        ));
        assert!(add(
            &mut a,
            &evidence(2),
            base + Duration::from_secs(61),
            16.0,
            RouteMode::Relay
        ));
        for second in 62..=121 {
            assert!(add(
                &mut a,
                &evidence(second),
                base + Duration::from_secs(second),
                2.0,
                RouteMode::Relay
            ));
        }
        assert_eq!(a.buckets().len(), 3);
        assert_eq!(a.buckets()[0].high_sample_count, 0);
        assert_eq!(a.buckets()[1].severe_sample_count, 1);
        assert_eq!(a.buckets()[2].observed_duration_ms, 1_000);
        assert!(!add(
            &mut a,
            &evidence(200),
            base + Duration::from_secs(200),
            2.0,
            RouteMode::Relay
        ));
    }
    #[test]
    fn provider_ids_never_pass_through_a_float_or_host_alias() {
        for value in [
            serde_json::json!(9_007_199_254_740_993_u64),
            serde_json::json!("9007199254740993"),
        ] {
            assert_eq!(provider_id(&value).as_deref(), Some("9007199254740993"));
        }
        for value in ["0", "-1", "1.0", "9223372036854775808", "01", "1\n"] {
            assert!(!valid_provider_id(value));
        }
        assert!(valid_provider_id("9223372036854775807"));
    }

    #[test]
    fn pooled_mean_uses_gauge_counts_not_session_means() {
        let base = Instant::now();
        let mut a = JitterAccumulator::default();
        a.start(Uuid::new_v4(), base, RouteMode::Direct);
        for n in 0..660 {
            assert!(add(
                &mut a,
                &evidence(n + 1),
                base + Duration::from_secs(60 + n),
                if n < 60 { 2.0 } else { 8.0 },
                RouteMode::Direct
            ));
        }
        let bucket = &a.buckets()[0];
        assert!((bucket.sum_ms / bucket.sample_count as f64 - 7.45454545).abs() < 0.00001);
    }
}
