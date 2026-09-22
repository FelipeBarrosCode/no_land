use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

use uuid::Uuid;

use crate::telemetry::{state::StateMachine, window::TelemetryWindow};

#[derive(Debug)]
pub struct RateLimiter {
    limit: usize,
    recent: VecDeque<Instant>,
}

impl RateLimiter {
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            recent: VecDeque::new(),
        }
    }

    pub fn allow(&mut self, now: Instant) -> bool {
        while self
            .recent
            .front()
            .is_some_and(|seen| now.saturating_duration_since(*seen) >= Duration::from_secs(1))
        {
            self.recent.pop_front();
        }
        if self.recent.len() >= self.limit {
            return false;
        }
        self.recent.push_back(now);
        true
    }
}

#[derive(Debug)]
pub struct Session {
    pub token: [u8; 32],
    pub window: TelemetryWindow,
    pub state: StateMachine,
    pub udp_rate: RateLimiter,
    registered_at: Instant,
}

#[derive(Debug)]
pub struct SessionRegistry {
    sessions: HashMap<Uuid, Session>,
    max_sessions: usize,
    udp_rate_limit: usize,
}

impl SessionRegistry {
    pub fn new(max_sessions: usize, udp_rate_limit: usize) -> Self {
        Self {
            sessions: HashMap::new(),
            max_sessions: max_sessions.max(1),
            udp_rate_limit,
        }
    }

    pub fn register(&mut self, session_id: Uuid, token: [u8; 32], now: Instant) {
        if self
            .sessions
            .get(&session_id)
            .is_some_and(|session| session.token == token)
        {
            return;
        }

        if !self.sessions.contains_key(&session_id) && self.sessions.len() >= self.max_sessions {
            if let Some(oldest) = self
                .sessions
                .iter()
                .min_by_key(|(_, session)| session.registered_at)
                .map(|(id, _)| *id)
            {
                self.sessions.remove(&oldest);
            }
        }

        self.sessions.insert(
            session_id,
            Session {
                token,
                window: TelemetryWindow::default(),
                state: StateMachine::default(),
                udp_rate: RateLimiter::new(self.udp_rate_limit),
                registered_at: now,
            },
        );
    }

    pub fn get_mut(&mut self, session_id: &Uuid) -> Option<&mut Session> {
        self.sessions.get_mut(session_id)
    }
}

impl Session {
    pub fn runtime(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.registered_at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::sample::MeasurementSample;

    #[test]
    fn same_token_reconnect_preserves_session_state() {
        let base = Instant::now();
        let session_id = Uuid::from_u128(1);
        let token = [7; 32];
        let mut registry = SessionRegistry::new(4, 20);
        registry.register(session_id, token, base);
        registry
            .get_mut(&session_id)
            .expect("registered session")
            .window
            .push(
                MeasurementSample {
                    sequence: 1,
                    rtt_ms: Some(18.0),
                    lost: false,
                },
                base,
            );

        registry.register(session_id, token, base + Duration::from_secs(10));
        let session = registry.get_mut(&session_id).expect("preserved session");
        assert_eq!(session.window.len(), 1);
        assert_eq!(
            session.runtime(base + Duration::from_secs(10)).as_secs(),
            10
        );
    }

    #[test]
    fn new_token_replaces_session_state() {
        let base = Instant::now();
        let session_id = Uuid::from_u128(2);
        let mut registry = SessionRegistry::new(4, 20);
        registry.register(session_id, [7; 32], base);
        registry
            .get_mut(&session_id)
            .expect("registered session")
            .window
            .push(
                MeasurementSample {
                    sequence: 1,
                    rtt_ms: Some(18.0),
                    lost: false,
                },
                base,
            );

        registry.register(session_id, [8; 32], base + Duration::from_secs(10));
        let session = registry.get_mut(&session_id).expect("replaced session");
        assert!(session.window.is_empty());
        assert_eq!(session.runtime(base + Duration::from_secs(10)).as_secs(), 0);
    }
}
