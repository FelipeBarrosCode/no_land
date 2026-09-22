use std::time::{Duration, Instant};

const CONNECTION_LOST_AFTER: Duration = Duration::from_secs(3);
const REARM_AFTER: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailSafeTransition {
    ConnectionLost,
    Recovered,
}

#[derive(Debug)]
pub(crate) struct FailSafeState {
    started_at: Instant,
    last_valid_ack: Option<Instant>,
    connection_lost: bool,
    recovery_since: Option<Instant>,
}

impl FailSafeState {
    pub(crate) fn new(started_at: Instant) -> Self {
        Self {
            started_at,
            last_valid_ack: None,
            connection_lost: false,
            recovery_since: None,
        }
    }

    pub(crate) fn tick(&mut self, now: Instant) -> Option<FailSafeTransition> {
        let health_anchor = self.last_valid_ack.unwrap_or(self.started_at);

        if now.saturating_duration_since(health_anchor) < CONNECTION_LOST_AFTER {
            return None;
        }

        self.recovery_since = None;
        if self.connection_lost {
            return None;
        }

        self.connection_lost = true;
        Some(FailSafeTransition::ConnectionLost)
    }

    pub(crate) fn observe_valid_ack(&mut self, now: Instant) -> Option<FailSafeTransition> {
        let previous_ack = self.last_valid_ack.replace(now);
        if !self.connection_lost {
            return None;
        }

        let health_was_interrupted = match previous_ack {
            Some(previous) => now.saturating_duration_since(previous) >= CONNECTION_LOST_AFTER,
            None => true,
        };
        if health_was_interrupted {
            self.recovery_since = Some(now);
            return None;
        }

        let recovery_since = *self.recovery_since.get_or_insert(now);
        if now.saturating_duration_since(recovery_since) < REARM_AFTER {
            return None;
        }

        self.connection_lost = false;
        self.recovery_since = None;
        Some(FailSafeTransition::Recovered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fail_safe_declares_initial_loss_after_three_seconds() {
        let base = Instant::now();
        let mut state = FailSafeState::new(base);

        assert_eq!(state.tick(base + Duration::from_millis(2_999)), None);
        assert_eq!(
            state.tick(base + Duration::from_secs(3)),
            Some(FailSafeTransition::ConnectionLost)
        );
    }

    #[test]
    fn connection_lost_is_emitted_only_once() {
        let base = Instant::now();
        let mut state = FailSafeState::new(base);
        state.observe_valid_ack(base);

        assert_eq!(
            state.tick(base + Duration::from_secs(3)),
            Some(FailSafeTransition::ConnectionLost)
        );
        assert_eq!(state.tick(base + Duration::from_secs(4)), None);
        assert_eq!(state.tick(base + Duration::from_secs(20)), None);
    }

    #[test]
    fn ten_seconds_of_ack_health_emits_recovery_and_rearms() {
        let base = Instant::now();
        let mut state = FailSafeState::new(base);
        state.observe_valid_ack(base);
        state.tick(base + Duration::from_secs(3));

        assert_eq!(state.observe_valid_ack(base + Duration::from_secs(4)), None);
        for second in 5..14 {
            assert_eq!(
                state.observe_valid_ack(base + Duration::from_secs(second)),
                None
            );
        }
        assert_eq!(
            state.observe_valid_ack(base + Duration::from_secs(14)),
            Some(FailSafeTransition::Recovered)
        );

        assert_eq!(
            state.tick(base + Duration::from_secs(17)),
            Some(FailSafeTransition::ConnectionLost)
        );
    }

    #[test]
    fn ack_gap_during_recovery_restarts_the_ten_second_window() {
        let base = Instant::now();
        let mut state = FailSafeState::new(base);
        state.observe_valid_ack(base);
        state.tick(base + Duration::from_secs(3));
        state.observe_valid_ack(base + Duration::from_secs(4));
        state.observe_valid_ack(base + Duration::from_secs(5));

        state.tick(base + Duration::from_secs(8));
        assert_eq!(state.observe_valid_ack(base + Duration::from_secs(9)), None);
        for second in 10..19 {
            assert_eq!(
                state.observe_valid_ack(base + Duration::from_secs(second)),
                None
            );
        }
        assert_eq!(
            state.observe_valid_ack(base + Duration::from_secs(19)),
            Some(FailSafeTransition::Recovered)
        );
    }
}
