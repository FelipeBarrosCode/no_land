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
pub mod persistence;
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
pub use persistence::*;
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

/// Provider-independent, versioned storage-format parameters. Provider transfer
/// profiles must never mutate these values.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct StorageFormatConfig {
    pub fastcdc_min: u64,
    pub fastcdc_avg: u64,
    pub fastcdc_max: u64,
    pub pack_target: u64,
    pub pack_max: u64,
}

impl Default for StorageFormatConfig {
    fn default() -> Self {
        Self {
            fastcdc_min: constants::FASTCDC_MIN,
            fastcdc_avg: constants::FASTCDC_AVG,
            fastcdc_max: constants::FASTCDC_MAX,
            pack_target: constants::PACK_TARGET,
            pack_max: constants::PACK_MAX,
        }
    }
}

#[cfg(test)]
mod storage_format_tests {
    use super::{constants, StorageFormatConfig};

    #[test]
    fn provider_independent_storage_format_defaults_are_locked() {
        let format = StorageFormatConfig::default();
        assert_eq!(format.fastcdc_min, 1024 * 1024);
        assert_eq!(format.fastcdc_avg, 4 * 1024 * 1024);
        assert_eq!(format.fastcdc_max, 8 * 1024 * 1024);
        assert_eq!(format.pack_target, 512 * 1024 * 1024);
        assert_eq!(format.pack_max, 1024 * 1024 * 1024);
        assert_eq!(format.fastcdc_min, constants::FASTCDC_MIN);
        assert_eq!(format.pack_target, constants::PACK_TARGET);
    }
}
