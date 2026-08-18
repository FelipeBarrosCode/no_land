//! Domain types for Noland application-state portability.
//!
//! This crate is the contract layer. Storage, observers, and restore engines
//! depend on these types and must not silently weaken the architectural rules
//! documented in the implementation plan.

pub mod catalog;
pub mod classify;
pub mod confidence;
pub mod error;
pub mod evidence;
pub mod identity;
pub mod logical_path;
pub mod manifest;
pub mod metrics;
pub mod operations;
pub mod paths;
pub mod policy;
pub mod process;
pub mod session;

pub use catalog::*;
pub use classify::*;
pub use confidence::*;
pub use error::*;
pub use evidence::*;
pub use identity::*;
pub use logical_path::*;
pub use manifest::*;
pub use operations::*;
pub use paths::*;
pub use policy::*;
pub use process::*;
pub use session::*;

/// FastCDC / packfile constants locked by the implementation plan.
pub mod constants {
    pub const FASTCDC_MIN: u64 = 1024 * 1024;
    pub const FASTCDC_AVG: u64 = 4 * 1024 * 1024;
    pub const FASTCDC_MAX: u64 = 8 * 1024 * 1024;
    pub const PACK_TARGET: u64 = 512 * 1024 * 1024;
    pub const PACK_MAX: u64 = 1024 * 1024 * 1024;
    pub const HASH_ALGORITHM: &str = "blake3";
    pub const CHUNK_ALGORITHM: &str = "fastcdc";
    pub const AEAD_ALGORITHM: &str = "xchacha20poly1305";
    pub const MANIFEST_SCHEMA_VERSION: u32 = 1;
    pub const DEFAULT_SHARED_STORAGE_ROOT: &str = "Noland Shared Storage";
    pub const CHECKPOINT_INTERVAL_SECS: u64 = 10 * 60;
    pub const DIRTY_BACKUP_INTERVAL_SECS: u64 = 15 * 60;
    pub const RETENTION_BUNDLE_VERSIONS: usize = 5;
    pub const STATE_DB_PATH: &str = "/var/lib/noland/state/state.db";
    pub const STATE_ROOT: &str = "/var/lib/noland/state";
    pub const RUN_ROOT: &str = "/run/noland";
    pub const RPC_SOCKET: &str = "/run/noland/state-agent.sock";
    pub const SHARED_STORAGE_ROOT_NAME: &str = "Noland Shared Storage";
}
