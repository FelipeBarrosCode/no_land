use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SidecarState {
    Idle,
    Starting,
    Running,
    Recovering,
    Stopping,
    Shutdown,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Health {
    Healthy,
    Degraded,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub session_id: Option<String>,
    pub state: SidecarState,
    pub health: Health,
    pub muted: bool,
    pub selected_device_id: Option<String>,
    pub active_device_id: Option<String>,
    pub active_sample_rate: u32,
    pub session_active: bool,
    pub last_error: Option<String>,
}

#[derive(Debug)]
pub struct StateMachine {
    state: SidecarState,
    health: Health,
    last_error: Option<String>,
}

impl Default for StateMachine {
    fn default() -> Self {
        Self {
            state: SidecarState::Idle,
            health: Health::Healthy,
            last_error: None,
        }
    }
}

impl StateMachine {
    pub fn state(&self) -> SidecarState {
        self.state
    }

    pub fn health(&self) -> Health {
        self.health
    }

    pub fn last_error(&self) -> Option<String> {
        self.last_error.clone()
    }

    pub fn transition(&mut self, next: SidecarState) -> Result<(), String> {
        let valid = matches!(
            (self.state, next),
            (SidecarState::Idle, SidecarState::Starting)
                | (SidecarState::Idle, SidecarState::Shutdown)
                | (SidecarState::Starting, SidecarState::Running)
                | (SidecarState::Starting, SidecarState::Recovering)
                | (SidecarState::Starting, SidecarState::Idle)
                | (SidecarState::Running, SidecarState::Recovering)
                | (SidecarState::Running, SidecarState::Stopping)
                | (SidecarState::Recovering, SidecarState::Running)
                | (SidecarState::Recovering, SidecarState::Stopping)
                | (SidecarState::Stopping, SidecarState::Idle)
                | (_, SidecarState::Shutdown)
        ) || self.state == next;
        if !valid {
            return Err(format!(
                "invalid sidecar state transition {:?} -> {:?}",
                self.state, next
            ));
        }
        self.state = next;
        Ok(())
    }

    pub fn healthy(&mut self) {
        self.health = Health::Healthy;
        self.last_error = None;
    }

    pub fn degraded(&mut self, error: impl Into<String>) {
        self.health = Health::Degraded;
        self.last_error = Some(error.into());
    }

    pub fn failed(&mut self, error: impl Into<String>) {
        self.health = Health::Failed;
        self.last_error = Some(error.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_session_and_recovery_transitions() {
        let mut state = StateMachine::default();
        state.transition(SidecarState::Starting).unwrap();
        state.transition(SidecarState::Running).unwrap();
        state.transition(SidecarState::Recovering).unwrap();
        state.transition(SidecarState::Running).unwrap();
        state.transition(SidecarState::Stopping).unwrap();
        state.transition(SidecarState::Idle).unwrap();
    }

    #[test]
    fn rejects_invalid_transition() {
        let mut state = StateMachine::default();
        assert!(state.transition(SidecarState::Running).is_err());
    }
}
