use serde::{Deserialize, Serialize};

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
}

impl Default for TransferTuning {
    fn default() -> Self {
        Self {
            max_parallel_uploads: 2,
            max_parallel_downloads: 4,
            max_bulk_files: 256,
            max_bulk_bytes: 512 * 1024 * 1024,
            max_attempts: 4,
            initial_backoff_ms: 500,
            max_backoff_ms: 30_000,
            rate_limit_cooldown_ms: 5_000,
            min_request_interval_ms: 0,
            rclone_transfers: 4,
            rclone_checkers: 4,
        }
    }
}

impl TransferTuning {
    /// Higher provider-neutral concurrency for foreground user-initiated transfers.
    pub fn throughput() -> Self {
        Self {
            max_parallel_uploads: 4,
            max_parallel_downloads: 8,
            max_bulk_files: 1_024,
            max_bulk_bytes: 1024 * 1024 * 1024,
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
            max_bulk_bytes: 128 * 1024 * 1024,
            min_request_interval_ms: 25,
            rclone_transfers: 2,
            rclone_checkers: 2,
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
        tuning
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
        assert!(gameplay.min_request_interval_ms > 0);
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
