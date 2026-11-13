pub mod artifact;
mod elf;
pub mod engine;
pub mod maintenance;
pub mod session;
pub mod store;

pub use noland_runtime_contracts::{DISPLAY, PROFILE, XAUTHORITY, XSOCKET};

pub fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}
