use std::time::{Duration, Instant};

use crate::classifier::{Classification, Quality, ReasonCode};

const BAD_PERSISTENCE: Duration = Duration::from_secs(5);
const RECOVERY_PERSISTENCE: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct StatusTransition {
    pub previous: Quality,
    pub current: Quality,
    pub reasons: Vec<ReasonCode>,
    pub alert_eligible: bool,
}

#[derive(Debug)]
pub struct StateMachine {
    published: Quality,
    bad_since: Option<Instant>,
    recovery_since: Option<Instant>,
    alert_eligible: bool,
}

impl Default for StateMachine {
    fn default() -> Self {
        Self {
            published: Quality::WarmingUp,
            bad_since: None,
            recovery_since: None,
            alert_eligible: true,
        }
    }
}

impl StateMachine {
    pub fn published(&self) -> Quality {
        self.published
    }

    pub fn bad_since(&self) -> Option<Instant> {
        self.bad_since
    }

    pub fn alert_eligible(&self) -> bool {
        self.alert_eligible
    }

    pub fn evaluate(
        &mut self,
        classification: &Classification,
        now: Instant,
    ) -> Option<StatusTransition> {
        let candidate = classification.status;

        if self.published == Quality::Bad {
            if candidate == Quality::Bad {
                self.recovery_since = None;
                return None;
            }

            let recovery_since = *self.recovery_since.get_or_insert(now);
            if now.saturating_duration_since(recovery_since) < RECOVERY_PERSISTENCE {
                return None;
            }

            let previous = self.published;
            self.published = candidate;
            self.bad_since = None;
            self.recovery_since = None;
            self.alert_eligible = true;
            return Some(StatusTransition {
                previous,
                current: candidate,
                reasons: classification.reasons.clone(),
                alert_eligible: true,
            });
        }

        if candidate == Quality::Bad {
            let bad_since = *self.bad_since.get_or_insert(now);
            if now.saturating_duration_since(bad_since) < BAD_PERSISTENCE {
                return None;
            }

            let previous = self.published;
            self.published = Quality::Bad;
            let alert_eligible = self.alert_eligible;
            self.alert_eligible = false;
            return Some(StatusTransition {
                previous,
                current: Quality::Bad,
                reasons: classification.reasons.clone(),
                alert_eligible,
            });
        }

        self.bad_since = None;
        self.recovery_since = None;
        if candidate == self.published {
            return None;
        }

        let previous = self.published;
        self.published = candidate;
        Some(StatusTransition {
            previous,
            current: candidate,
            reasons: classification.reasons.clone(),
            alert_eligible: self.alert_eligible,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classifier::{Classification, Quality, ReasonCode};

    fn classification(status: Quality) -> Classification {
        Classification {
            status,
            stability: status,
            latency: status,
            reasons: (status == Quality::Bad)
                .then_some(ReasonCode::HighLatency)
                .into_iter()
                .collect(),
        }
    }

    #[test]
    fn two_second_bad_candidate_is_not_published() {
        let base = Instant::now();
        let mut state = StateMachine::default();
        state.evaluate(&classification(Quality::Great), base);
        state.evaluate(&classification(Quality::Bad), base + Duration::from_secs(1));
        let transition =
            state.evaluate(&classification(Quality::Bad), base + Duration::from_secs(3));

        assert!(transition.is_none());
        assert_eq!(state.published(), Quality::Great);
        assert!(state.alert_eligible());
    }

    #[test]
    fn six_second_bad_candidate_emits_only_one_alert() {
        let base = Instant::now();
        let mut state = StateMachine::default();
        state.evaluate(&classification(Quality::Great), base);
        state.evaluate(&classification(Quality::Bad), base + Duration::from_secs(1));
        let transition = state
            .evaluate(&classification(Quality::Bad), base + Duration::from_secs(7))
            .expect("BAD should publish");

        assert_eq!(transition.current, Quality::Bad);
        assert!(transition.alert_eligible);
        assert!(state
            .evaluate(&classification(Quality::Bad), base + Duration::from_secs(8))
            .is_none());
        assert!(!state.alert_eligible());
    }

    #[test]
    fn ten_second_recovery_rearms_alerting() {
        let base = Instant::now();
        let mut state = StateMachine::default();
        state.evaluate(&classification(Quality::Great), base);
        state.evaluate(&classification(Quality::Bad), base + Duration::from_secs(1));
        state.evaluate(&classification(Quality::Bad), base + Duration::from_secs(7));

        state.evaluate(
            &classification(Quality::Good),
            base + Duration::from_secs(8),
        );
        assert!(state
            .evaluate(
                &classification(Quality::Good),
                base + Duration::from_secs(17)
            )
            .is_none());
        let recovery = state
            .evaluate(
                &classification(Quality::Good),
                base + Duration::from_secs(18),
            )
            .expect("recovery should publish");
        assert_eq!(recovery.current, Quality::Good);
        assert!(state.alert_eligible());

        state.evaluate(
            &classification(Quality::Bad),
            base + Duration::from_secs(19),
        );
        let next_bad = state
            .evaluate(
                &classification(Quality::Bad),
                base + Duration::from_secs(24),
            )
            .expect("next BAD episode should publish");
        assert!(next_bad.alert_eligible);
    }
}
