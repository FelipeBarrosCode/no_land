//! Shared, versioned contracts used by the desktop client and host network agent.

pub mod control;
pub mod errors;
pub mod evaluation;
pub mod events;
pub mod probe;
pub mod state;

pub const CONTROL_PROTOCOL_VERSION: u16 = 1;
pub const NETWORK_STATE_SCHEMA_VERSION: u16 = 1;
pub const EVENT_SCHEMA_VERSION: u16 = 1;
