use serde::{Deserialize, Serialize};

use crate::{ProviderCapabilities, ProviderKind};

#[cfg(test)]
const MIB: u64 = 1024 * 1024;
#[cfg(test)]
const MEMORY_PER_TRANSFER: u64 = 128 * MIB;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferWorkload {
    Balanced,
    Throughput,
    GameplaySafe,
}

/// Provider-neutral transfer controls. Defaults intentionally favor reliability
/// and low remote API pressure over maximum throughput.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct TransferTuning {
    pub max_parallel_uploads: usize,
    pub max_parallel_downloads: usize,
    pub max_bulk_files: usize,
    pub max_bulk_bytes: u64,
    pub max_attempts: u32,
    pub initial_backoff_ms: u64,
    pub max_backoff_ms: u64,
    pub rate_limit_cooldown_ms: u64,
    pub min_request_interval_ms: u64,
    pub rclone_transfers: usize,
    pub rclone_checkers: usize,
    pub rclone_download_streams: usize,
}

impl Default for TransferTuning {
    fn default() -> Self {
        Self {
            max_parallel_uploads: 2,
            max_parallel_downloads: 4,
            max_bulk_files: 256,
            max_bulk_bytes: 2 * 1024 * 1024 * 1024,
            max_attempts: 4,
            initial_backoff_ms: 500,
            max_backoff_ms: 30_000,
            rate_limit_cooldown_ms: 5_000,
            min_request_interval_ms: 0,
            rclone_transfers: 4,
            rclone_checkers: 4,
            rclone_download_streams: 4,
        }
    }
}

impl TransferTuning {
    /// Resolves a hidden, provider-aware transfer budget on the machine doing
    /// the work. The desktop never needs to ask the user for a worker count.
    pub fn automatic(provider: ProviderKind, workload: TransferWorkload) -> Self {
        let cpus = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(4);
        Self::for_resources(provider, workload, cpus, available_memory_bytes())
    }

    pub fn for_resources(
        provider: ProviderKind,
        workload: TransferWorkload,
        logical_cpus: usize,
        available_memory: Option<u64>,
    ) -> Self {
        let cpus = logical_cpus.max(1);
        let cpu_budget = match workload {
            TransferWorkload::Balanced => cpus.max(2),
            TransferWorkload::Throughput => cpus.saturating_mul(2).max(4),
            TransferWorkload::GameplaySafe => cpus.div_ceil(4).max(1),
        };
        let capabilities = ProviderCapabilities::for_provider(provider);
        let memory_per_transfer = capabilities.estimated_memory_per_transfer.max(1);
        let memory_budget = available_memory
            .map(|bytes| {
                usize::try_from((bytes / memory_per_transfer).max(1)).unwrap_or(usize::MAX)
            })
            .unwrap_or(cpu_budget);
        let upload_budget = cpu_budget
            .min(memory_budget)
            .min(capabilities.max_upload_concurrency)
            .max(1);
        let download_budget = cpu_budget
            .min(memory_budget)
            .min(capabilities.max_download_concurrency)
            .max(1);
        let transfer_budget = upload_budget.max(download_budget);

        let mut tuning = match workload {
            TransferWorkload::Balanced => Self::default(),
            TransferWorkload::Throughput => Self::throughput(),
            TransferWorkload::GameplaySafe => Self::gameplay_safe(),
        };
        tuning.max_parallel_uploads = upload_budget;
        tuning.max_parallel_downloads = download_budget;
        tuning.rclone_transfers = upload_budget;
        tuning.rclone_checkers = transfer_budget;
        tuning.rclone_download_streams = download_budget.min(4);
        tuning.normalized()
    }

    /// Higher provider-neutral concurrency for foreground user-initiated transfers.
    pub fn throughput() -> Self {
        Self {
            max_parallel_uploads: 4,
            max_parallel_downloads: 8,
            max_bulk_files: 1_024,
            max_bulk_bytes: 4 * 1024 * 1024 * 1024,
            rclone_transfers: 8,
            rclone_checkers: 8,
            ..Self::default()
        }
    }

    /// Limits disk, CPU, and network contention while an application session is active.
    pub fn gameplay_safe() -> Self {
        Self {
            max_parallel_uploads: 1,
            max_parallel_downloads: 2,
            max_bulk_files: 128,
            max_bulk_bytes: 1024 * 1024 * 1024,
            min_request_interval_ms: 25,
            rclone_transfers: 2,
            rclone_checkers: 2,
            rclone_download_streams: 1,
            ..Self::default()
        }
    }

    pub fn normalized(&self) -> Self {
        let mut tuning = self.clone();
        tuning.max_parallel_uploads = tuning.max_parallel_uploads.max(1);
        tuning.max_parallel_downloads = tuning.max_parallel_downloads.max(1);
        tuning.max_bulk_files = tuning.max_bulk_files.max(1);
        tuning.max_bulk_bytes = tuning.max_bulk_bytes.max(1);
        tuning.max_attempts = tuning.max_attempts.max(1);
        tuning.max_backoff_ms = tuning.max_backoff_ms.max(tuning.initial_backoff_ms);
        tuning.rclone_transfers = tuning.rclone_transfers.max(1);
        tuning.rclone_checkers = tuning.rclone_checkers.max(1);
        tuning.rclone_download_streams = tuning.rclone_download_streams.max(1);
        tuning
    }

    pub fn parallel_download_processes(&self) -> usize {
        self.max_parallel_downloads
            .max(1)
            .div_ceil(self.rclone_download_streams.max(1))
    }

    pub fn backoff_ms(&self, failed_attempt: u32, rate_limited: bool) -> u64 {
        let exponent = failed_attempt.saturating_sub(1).min(31);
        let exponential = self
            .initial_backoff_ms
            .saturating_mul(1u64 << exponent)
            .min(self.max_backoff_ms);
        if rate_limited {
            exponential.max(self.rate_limit_cooldown_ms)
        } else {
            exponential
        }
    }
}

fn available_memory_bytes() -> Option<u64> {
    let cgroup_v2_available = read_trimmed("/sys/fs/cgroup/memory.max").and_then(|limit| {
        if limit == "max" {
            return None;
        }
        let limit = limit.parse::<u64>().ok()?;
        let current = read_trimmed("/sys/fs/cgroup/memory.current")?
            .parse::<u64>()
            .ok()?;
        Some(limit.saturating_sub(current))
    });
    let cgroup_v1_available =
        read_trimmed("/sys/fs/cgroup/memory/memory.limit_in_bytes").and_then(|limit| {
            let limit = limit.parse::<u64>().ok()?;
            let current = read_trimmed("/sys/fs/cgroup/memory/memory.usage_in_bytes")?
                .parse::<u64>()
                .ok()?;
            Some(limit.saturating_sub(current))
        });
    let cgroup_available = cgroup_v2_available.or(cgroup_v1_available);
    let host_available = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|contents| {
            contents.lines().find_map(|line| {
                let kib = line
                    .strip_prefix("MemAvailable:")?
                    .split_whitespace()
                    .next()?;
                kib.parse::<u64>().ok()?.checked_mul(1024)
            })
        });
    match (cgroup_available, host_available) {
        (Some(cgroup), Some(host)) => Some(cgroup.min(host)),
        (Some(bytes), None) | (None, Some(bytes)) => Some(bytes),
        (None, None) => None,
    }
}

fn read_trimmed(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
}

/// Credential-free identity suitable for provider/root-scoped caches.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ProviderRootIdentity {
    pub provider: String,
    pub backend_type: String,
    pub remote_name: String,
    pub root: String,
}

impl ProviderRootIdentity {
    pub fn new(
        provider: impl Into<String>,
        backend_type: impl Into<String>,
        remote_name: impl Into<String>,
        root: impl Into<String>,
    ) -> Self {
        Self {
            provider: provider.into(),
            backend_type: backend_type.into(),
            remote_name: remote_name.into(),
            root: normalize_root(&root.into()),
        }
    }

    /// Stable, path-safe key. Credentials are deliberately excluded.
    pub fn cache_key(&self) -> String {
        let canonical = format!(
            "{}\0{}\0{}\0{}",
            self.provider, self.backend_type, self.remote_name, self.root
        );
        format!("remote-{:016x}", fnv1a64(canonical.as_bytes()))
    }
}

fn normalize_root(root: &str) -> String {
    root.trim().trim_matches('/').replace('\\', "/")
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RemoteErrorClass {
    Retryable,
    RateLimited,
    Authentication,
    NotFound,
    ImmutableConflict,
    Permanent,
}

impl RemoteErrorClass {
    pub fn is_retryable(self) -> bool {
        matches!(self, Self::Retryable | Self::RateLimited)
    }

    pub fn is_rate_limited(self) -> bool {
        self == Self::RateLimited
    }
}

/// Classifies common rclone/backend failures without relying on one provider.
pub fn classify_remote_error(exit_code: Option<i32>, stderr: &str) -> RemoteErrorClass {
    let message = stderr.to_ascii_lowercase();

    if contains_any(
        &message,
        &[
            "immutable file modified",
            "immutable file changed",
            "can't modify immutable",
            "cannot modify immutable",
        ],
    ) {
        return RemoteErrorClass::ImmutableConflict;
    }
    if contains_any(
        &message,
        &[
            "too many requests",
            "rate limit",
            "ratelimit",
            "status code: 429",
            "http status 429",
            "http error 429",
            "error 429",
            "quota exceeded",
            "slowdown",
        ],
    ) {
        return RemoteErrorClass::RateLimited;
    }
    if contains_any(
        &message,
        &[
            "unauthorized",
            "forbidden",
            "invalid_grant",
            "invalid credentials",
            "authentication failed",
            "access denied",
            "status code: 401",
            "status code: 403",
        ],
    ) {
        return RemoteErrorClass::Authentication;
    }
    if contains_any(
        &message,
        &[
            "not found",
            "directory not found",
            "object not found",
            "no such file",
            "status code: 404",
        ],
    ) {
        return RemoteErrorClass::NotFound;
    }
    if contains_any(
        &message,
        &[
            "timeout",
            "timed out",
            "connection reset",
            "connection refused",
            "connection closed",
            "temporary failure",
            "temporarily unavailable",
            "try again",
            "unexpected eof",
            "broken pipe",
            "tls handshake timeout",
            "i/o timeout",
            "status code: 500",
            "status code: 502",
            "status code: 503",
            "status code: 504",
            "http status 500",
            "http status 502",
            "http status 503",
            "http status 504",
            "http error 500",
            "http error 502",
            "http error 503",
            "http error 504",
        ],
    ) {
        return RemoteErrorClass::Retryable;
    }

    let _ = exit_code;
    RemoteErrorClass::Permanent
}

fn contains_any(message: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| message.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conservative_defaults_normalize_limits() {
        let defaults = TransferTuning::default();
        assert_eq!(defaults.max_parallel_uploads, 2);
        assert_eq!(defaults.max_bulk_bytes, 2 * 1024 * 1024 * 1024);
        assert_eq!(defaults.rclone_transfers, 4);
        assert!(defaults.max_attempts >= 3);

        let mut invalid = defaults;
        invalid.max_parallel_uploads = 0;
        invalid.max_attempts = 0;
        invalid.rclone_transfers = 0;
        let normalized = invalid.normalized();
        assert_eq!(normalized.max_parallel_uploads, 1);
        assert_eq!(normalized.max_attempts, 1);
        assert_eq!(normalized.rclone_transfers, 1);
    }

    #[test]
    fn workload_profiles_trade_throughput_for_gameplay_isolation() {
        let throughput = TransferTuning::throughput();
        let gameplay = TransferTuning::gameplay_safe();
        assert!(throughput.max_parallel_uploads > gameplay.max_parallel_uploads);
        assert!(throughput.rclone_transfers > gameplay.rclone_transfers);
        assert_eq!(throughput.max_bulk_bytes, 4 * 1024 * 1024 * 1024);
        assert_eq!(throughput.rclone_transfers, 8);
        assert_eq!(throughput.rclone_checkers, 8);
        assert_eq!(gameplay.max_bulk_bytes, 1024 * 1024 * 1024);
        assert!(gameplay.min_request_interval_ms > 0);
    }

    #[test]
    fn automatic_budget_uses_resources_and_provider_limits() {
        let object = TransferTuning::for_resources(
            ProviderKind::AmazonS3,
            TransferWorkload::Throughput,
            16,
            Some(16 * 1024 * MIB),
        );
        assert_eq!(object.max_parallel_uploads, 32);
        assert_eq!(object.max_parallel_downloads, 32);

        let drive = TransferTuning::for_resources(
            ProviderKind::GoogleDrive,
            TransferWorkload::Throughput,
            64,
            Some(64 * 1024 * MIB),
        );
        assert_eq!(drive.max_parallel_uploads, 16);
        assert_eq!(drive.rclone_transfers, 16);
        assert_eq!(drive.rclone_download_streams, 4);
        assert_eq!(drive.parallel_download_processes(), 4);
    }

    #[test]
    fn automatic_budget_respects_memory_and_gameplay_pressure() {
        let memory_limited = TransferTuning::for_resources(
            ProviderKind::AmazonS3,
            TransferWorkload::Throughput,
            32,
            Some(2 * MEMORY_PER_TRANSFER),
        );
        assert_eq!(memory_limited.max_parallel_uploads, 2);

        let gameplay = TransferTuning::for_resources(
            ProviderKind::AmazonS3,
            TransferWorkload::GameplaySafe,
            16,
            None,
        );
        assert_eq!(gameplay.max_parallel_uploads, 4);
        assert_eq!(gameplay.rclone_download_streams, 4);
        assert_eq!(gameplay.parallel_download_processes(), 1);
        assert!(gameplay.max_parallel_uploads < 32);
    }

    #[test]
    fn backoff_is_bounded_and_rate_limits_have_a_floor() {
        let tuning = TransferTuning::default();
        assert_eq!(tuning.backoff_ms(1, false), 500);
        assert_eq!(tuning.backoff_ms(2, false), 1_000);
        assert_eq!(tuning.backoff_ms(99, false), 30_000);
        assert_eq!(tuning.backoff_ms(1, true), 5_000);
    }

    #[test]
    fn provider_root_cache_key_is_stable_and_root_sensitive() {
        let a = ProviderRootIdentity::new("generic_s3", "s3", "cloud", "/bucket/repo/");
        let b = ProviderRootIdentity::new("generic_s3", "s3", "cloud", "bucket/repo");
        let c = ProviderRootIdentity::new("generic_s3", "s3", "cloud", "bucket/other");
        assert_eq!(a.cache_key(), b.cache_key());
        assert_ne!(a.cache_key(), c.cache_key());
        assert!(!a.cache_key().contains('/'));
    }

    #[test]
    fn classifies_retryable_and_terminal_failures() {
        assert_eq!(
            classify_remote_error(Some(1), "HTTP error 429: too many requests"),
            RemoteErrorClass::RateLimited
        );
        assert_eq!(
            classify_remote_error(Some(1), "connection reset by peer"),
            RemoteErrorClass::Retryable
        );
        assert_eq!(
            classify_remote_error(Some(1), "401 Unauthorized"),
            RemoteErrorClass::Authentication
        );
        assert_eq!(
            classify_remote_error(Some(1), "immutable file modified"),
            RemoteErrorClass::ImmutableConflict
        );
    }
}
