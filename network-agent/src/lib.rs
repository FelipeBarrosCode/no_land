use std::sync::Arc;

use tokio::sync::Mutex;

pub mod classifier;
pub mod config;
pub mod control;
pub mod probe;
pub mod remote_control;
pub mod session;
pub mod telemetry;
pub mod transport;
pub mod turn_bridge;
pub mod turn_manager;

pub use session::SessionRegistry;

pub type SharedState = Arc<Mutex<SessionRegistry>>;

pub fn shared_state(max_sessions: usize, udp_rate_limit: usize) -> SharedState {
    Arc::new(Mutex::new(SessionRegistry::new(
        max_sessions,
        udp_rate_limit,
    )))
}
