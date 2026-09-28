use crate::state::{
    ConnectionEvaluation, EvaluationComparison, EvaluationReason, PathEvaluation, PathMetrics,
    TransportKind,
};
use uuid::Uuid;

pub const GAMING_POLICY_VERSION: &str = "gaming-v1";

#[derive(Debug, Clone, Copy)]
pub struct EvaluationPolicy {
    pub minimum_samples: u32,
    pub target_samples: u32,
    pub maximum_sample_age_ms: u64,
    pub latency_deadband_ms: f64,
    pub latency_deadband_ratio: f64,
    pub stability_deadband: f64,
    pub provisioning_turn_threshold: f64,
    pub runtime_turn_threshold: f64,
    pub runtime_direct_threshold: f64,
}

impl Default for EvaluationPolicy {
    fn default() -> Self {
        Self {
            minimum_samples: 45,
            target_samples: 60,
            maximum_sample_age_ms: 3_000,
            latency_deadband_ms: 3.0,
            latency_deadband_ratio: 0.05,
            stability_deadband: 0.05,
            provisioning_turn_threshold: 0.08,
            runtime_turn_threshold: 0.12,
            runtime_direct_threshold: -0.12,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvaluationMode {
    Provisioning,
    Runtime { current: TransportKind },
}

pub fn evaluate_paths(
    direct_metrics: PathMetrics,
    relay_metrics: PathMetrics,
    direct_available: bool,
    relay_available: bool,
    mode: EvaluationMode,
    evaluated_at: String,
    policy: EvaluationPolicy,
) -> ConnectionEvaluation {
    let direct = evaluate_path(direct_metrics, policy);
    let cloudflare_turn = evaluate_path(relay_metrics, policy);

    let (selected, reason, comparison) = if !direct_available && relay_available {
        (
            Some(TransportKind::CloudflareTurn),
            EvaluationReason::DirectUnavailable,
            EvaluationComparison::default(),
        )
    } else if direct_available && !relay_available {
        (
            Some(TransportKind::Direct),
            EvaluationReason::RelayUnavailable,
            EvaluationComparison::default(),
        )
    } else if !direct_available && !relay_available {
        (
            None,
            EvaluationReason::InsufficientSamples,
            EvaluationComparison::default(),
        )
    } else if !direct.scoreable || !cloudflare_turn.scoreable {
        let selected = match mode {
            EvaluationMode::Provisioning => Some(TransportKind::Direct),
            EvaluationMode::Runtime { current } => Some(current),
        };
        (
            selected,
            EvaluationReason::InsufficientSamples,
            EvaluationComparison::default(),
        )
    } else {
        compare(&direct, &cloudflare_turn, mode, policy)
    };

    ConnectionEvaluation {
        evaluation_id: Uuid::new_v4(),
        policy_version: GAMING_POLICY_VERSION.to_string(),
        evaluated_at,
        selected,
        reason,
        direct,
        cloudflare_turn,
        comparison,
    }
}

pub fn evaluate_path(metrics: PathMetrics, policy: EvaluationPolicy) -> PathEvaluation {
    let received_count = if metrics.received_count == 0 && metrics.sample_count > 0 {
        // Backward compatibility for persisted v1 metrics.
        metrics.sample_count
    } else {
        metrics.received_count
    };
    let scoreable = received_count >= policy.minimum_samples
        && metrics.sample_age_ms <= policy.maximum_sample_age_ms
        && metrics.median_rtt_ms.is_some();
    let sample_confidence =
        (received_count as f64 / policy.target_samples.max(1) as f64).clamp(0.0, 1.0);
    let freshness_confidence = if metrics.sample_age_ms >= policy.maximum_sample_age_ms {
        0.0
    } else {
        1.0 - metrics.sample_age_ms as f64 / policy.maximum_sample_age_ms.max(1) as f64
    };
    let confidence = sample_confidence.min(freshness_confidence);
    let stability_penalty = stability_penalty(&metrics);
    PathEvaluation {
        metrics,
        stability_penalty,
        confidence,
        scoreable,
    }
}

fn stability_penalty(metrics: &PathMetrics) -> f64 {
    let loss = (metrics.loss_percent / 3.0).clamp(0.0, 1.0);
    let jitter = (metrics.jitter_ms / 20.0).clamp(0.0, 1.0);
    let spikes = (metrics.spike_percent / 10.0).clamp(0.0, 1.0);
    match metrics.reordering_percent {
        Some(reordering) => {
            0.40 * loss + 0.35 * jitter + 0.20 * spikes + 0.05 * (reordering / 2.0).clamp(0.0, 1.0)
        }
        None => 0.4211 * loss + 0.3684 * jitter + 0.2105 * spikes,
    }
}

fn compare(
    direct: &PathEvaluation,
    relay: &PathEvaluation,
    mode: EvaluationMode,
    policy: EvaluationPolicy,
) -> (
    Option<TransportKind>,
    EvaluationReason,
    EvaluationComparison,
) {
    let direct_latency = direct.metrics.median_rtt_ms.unwrap_or_default();
    let relay_latency = relay.metrics.median_rtt_ms.unwrap_or_default();
    let latency_difference = direct_latency - relay_latency;
    let latency_deadband = policy
        .latency_deadband_ms
        .max(direct_latency.min(relay_latency) * policy.latency_deadband_ratio);
    let latency_advantage = if latency_difference.abs() <= latency_deadband {
        0.0
    } else {
        (latency_difference / direct_latency.max(relay_latency).max(30.0)).clamp(-1.0, 1.0)
    };

    let stability_difference = direct.stability_penalty - relay.stability_penalty;
    let stability_advantage = if stability_difference.abs() <= policy.stability_deadband {
        0.0
    } else {
        (stability_difference
            / direct
                .stability_penalty
                .max(relay.stability_penalty)
                .max(0.15))
        .clamp(-1.0, 1.0)
    };

    let total_gap = latency_advantage.abs() + stability_advantage.abs();
    let (latency_weight, stability_weight) = if total_gap == 0.0 {
        (0.0, 0.0)
    } else {
        (
            latency_advantage.abs() / total_gap,
            stability_advantage.abs() / total_gap,
        )
    };
    let confidence = direct.confidence.min(relay.confidence);
    let turn_advantage =
        (latency_weight * latency_advantage + stability_weight * stability_advantage) * confidence;
    let comparison = EvaluationComparison {
        latency_advantage,
        stability_advantage,
        latency_weight,
        stability_weight,
        confidence,
        turn_advantage,
    };

    let (selected, reason) = match mode {
        EvaluationMode::Provisioning if turn_advantage >= policy.provisioning_turn_threshold => (
            TransportKind::CloudflareTurn,
            winning_reason(latency_advantage, stability_advantage, true),
        ),
        EvaluationMode::Provisioning => {
            let reason = if turn_advantage == 0.0 {
                EvaluationReason::PathsEquivalent
            } else {
                winning_reason(latency_advantage, stability_advantage, false)
            };
            (TransportKind::Direct, reason)
        }
        EvaluationMode::Runtime {
            current: TransportKind::Direct,
        } if turn_advantage >= policy.runtime_turn_threshold => (
            TransportKind::CloudflareTurn,
            winning_reason(latency_advantage, stability_advantage, true),
        ),
        EvaluationMode::Runtime {
            current: TransportKind::CloudflareTurn,
        } if turn_advantage <= policy.runtime_direct_threshold => (
            TransportKind::Direct,
            winning_reason(latency_advantage, stability_advantage, false),
        ),
        EvaluationMode::Runtime { current } => (current, EvaluationReason::PathsEquivalent),
    };
    (Some(selected), reason, comparison)
}

fn winning_reason(
    latency_advantage: f64,
    stability_advantage: f64,
    relay_wins: bool,
) -> EvaluationReason {
    let latency_wins = if relay_wins {
        latency_advantage > 0.0
    } else {
        latency_advantage < 0.0
    };
    let stability_wins = if relay_wins {
        stability_advantage > 0.0
    } else {
        stability_advantage < 0.0
    };
    match (relay_wins, latency_wins, stability_wins) {
        (true, true, true) => EvaluationReason::RelayWinsBoth,
        (true, true, false) => EvaluationReason::RelayLowerLatency,
        (true, false, true) => EvaluationReason::RelayMoreStable,
        (false, true, true) => EvaluationReason::DirectWinsBoth,
        (false, true, false) => EvaluationReason::DirectLowerLatency,
        (false, false, true) => EvaluationReason::DirectMoreStable,
        _ => EvaluationReason::PathsEquivalent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics(latency: f64, jitter: f64, loss: f64, spikes: f64) -> PathMetrics {
        PathMetrics {
            sample_count: 60,
            sent_count: 60,
            received_count: 60,
            lost_count: 0,
            sample_age_ms: 0,
            median_rtt_ms: Some(latency),
            p95_rtt_ms: Some(latency + jitter),
            p99_rtt_ms: Some(latency + jitter * 2.0),
            jitter_ms: jitter,
            loss_percent: loss,
            spike_percent: spikes,
            reordering_percent: None,
        }
    }

    #[test]
    fn relay_is_selected_when_latency_is_similar_but_stability_is_much_better() {
        let result = evaluate_paths(
            metrics(32.0, 18.0, 2.0, 9.0),
            metrics(34.0, 2.0, 0.0, 0.0),
            true,
            true,
            EvaluationMode::Provisioning,
            "2026-09-26T20:00:00Z".into(),
            EvaluationPolicy::default(),
        );
        assert_eq!(result.selected, Some(TransportKind::CloudflareTurn));
        assert_eq!(result.reason, EvaluationReason::RelayMoreStable);
    }

    #[test]
    fn direct_is_default_when_paths_are_equivalent() {
        let result = evaluate_paths(
            metrics(25.0, 1.0, 0.0, 0.0),
            metrics(26.0, 1.0, 0.0, 0.0),
            true,
            true,
            EvaluationMode::Provisioning,
            "2026-09-26T20:00:00Z".into(),
            EvaluationPolicy::default(),
        );
        assert_eq!(result.selected, Some(TransportKind::Direct));
        assert_eq!(result.reason, EvaluationReason::PathsEquivalent);
    }

    #[test]
    fn runtime_hysteresis_keeps_the_current_path_for_a_small_advantage() {
        let result = evaluate_paths(
            metrics(32.0, 2.5, 0.1, 0.5),
            metrics(30.0, 2.0, 0.0, 0.0),
            true,
            true,
            EvaluationMode::Runtime {
                current: TransportKind::Direct,
            },
            "2026-09-26T20:00:00Z".into(),
            EvaluationPolicy::default(),
        );
        assert_eq!(result.selected, Some(TransportKind::Direct));
    }

    #[test]
    fn missing_relay_samples_keep_direct_during_provisioning() {
        let mut relay = metrics(20.0, 1.0, 0.0, 0.0);
        relay.sample_count = 5;
        relay.received_count = 5;
        relay.lost_count = 55;
        let result = evaluate_paths(
            metrics(30.0, 2.0, 0.0, 0.0),
            relay,
            true,
            true,
            EvaluationMode::Provisioning,
            "now".into(),
            EvaluationPolicy::default(),
        );
        assert_eq!(result.selected, Some(TransportKind::Direct));
        assert_eq!(result.reason, EvaluationReason::InsufficientSamples);
    }
}
