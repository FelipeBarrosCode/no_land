use serde::{Deserialize, Serialize};

use super::MoonlightError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionState {
    Idle,
    Preparing,
    Launching,
    CreatingSurface,
    Connecting,
    Streaming,
    Reconnecting,
    Stopping,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionSignal {
    StartRequested,
    PreparationCompleted,
    LaunchCompleted,
    SurfaceCreated,
    ConnectionStarted,
    ConnectionEstablished,
    StopRequested,
    ConnectionLost,
    ReconnectRequested,
    Stopped,
}

pub fn transition(
    current: &SessionState,
    signal: SessionSignal,
) -> Result<SessionState, MoonlightError> {
    use SessionSignal::*;
    use SessionState::*;

    let next = match (current, &signal) {
        (Idle, StartRequested) => Preparing,
        (Preparing, PreparationCompleted) => Launching,
        (Launching, LaunchCompleted) => CreatingSurface,
        (CreatingSurface, SurfaceCreated) => Connecting,
        (Connecting, ConnectionStarted) => Connecting,
        (Connecting, ConnectionEstablished) => Streaming,
        (Streaming, StopRequested) => Stopping,
        (Streaming, ConnectionLost) => Reconnecting,
        (Reconnecting, ReconnectRequested) => Connecting,
        (Preparing, StopRequested)
        | (Launching, StopRequested)
        | (CreatingSurface, StopRequested)
        | (Connecting, StopRequested)
        | (Reconnecting, StopRequested) => Stopping,
        (Stopping, Stopped) => Idle,
        _ => {
            return Err(MoonlightError::InvalidSessionTransition {
                from: current.clone(),
                signal,
            })
        }
    };

    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_nominal_startup_flow() {
        let state = transition(&SessionState::Idle, SessionSignal::StartRequested).unwrap();
        let state = transition(&state, SessionSignal::PreparationCompleted).unwrap();
        let state = transition(&state, SessionSignal::LaunchCompleted).unwrap();
        let state = transition(&state, SessionSignal::SurfaceCreated).unwrap();
        let state = transition(&state, SessionSignal::ConnectionEstablished).unwrap();
        assert_eq!(state, SessionState::Streaming);
    }

    #[test]
    fn rejects_double_start() {
        let error =
            transition(&SessionState::Preparing, SessionSignal::StartRequested).unwrap_err();
        assert!(matches!(
            error,
            MoonlightError::InvalidSessionTransition { .. }
        ));
    }
}
