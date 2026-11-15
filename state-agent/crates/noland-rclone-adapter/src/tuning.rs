use serde::{Deserialize, Serialize};

use crate::{ProviderCapabilities, ProviderKind, WorkerResources};

#[cfg(test)]
const MIB: u64 = 1024 * 1024;

// Bound logical staging to two large-object waves per admitted transfer, rather
// than an operation-wide fixed 1/2/4 GiB cap. This is not a pack-format limit.
const STAGED_BYTES_PER_TRANSFER: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferWorkload {
    Balanced,
    Throughput,
    GameplaySafe,
}

impl TransferWorkload {
    /// Active uploads/downloads temporarily bypass gameplay throttling.
    ///
    /// Resolve an operation-local value, never change the caller's normal
    /// policy. Completion, errors, cancellation and overlapping transfers need
    /// no global flag or cleanup to restore gameplay protection.
    pub const fn for_active_transfer(self) -> Self {
        match self {
            Self::GameplaySafe => Self::Throughput,
            other => other,
        }
    }
}

/// Provider-neutral transfer controls. The explicit/default profiles retain
/// their historical wire defaults; automatic operation budgets use resources.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct TransferTuning {
    pub max_parallel_uploads: usize,
    pub max_parallel_downloads: usize,
    pub max_bulk_files: usize,
    /// Operation-wide logical staged ciphertext budget. This is not a bound on
    /// total RSS or disk retention: sources, codec workspaces, backend buffers,
    /// retries and durable staged files have separate lifetimes. Callers handle
    /// a single object larger than this budget with exclusive admission.
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
    /// Raw currently available memory, before the shared admission partition.
    /// Pass this directly to resource helpers; do not reserve headroom twice.
    pub fn preparation_memory_bytes() -> Option<u64> {
        crate::resources::available_memory_bytes()
    }

    /// Resolves a hidden, provider-aware transfer budget on the machine doing
    /// the work. The desktop never needs to ask the user for a worker count.
    pub fn automatic(provider: ProviderKind, workload: TransferWorkload) -> Self {
        Self::from_worker_resources(provider, workload, WorkerResources::detect())
    }

    /// Pure resource model for a host without an IO-pressure signal.
    /// `available_memory` is raw availability, not an already-halved allowance.
    /// Use [`Self::from_worker_resources`] to preserve a known IO-pressure signal.
    pub fn for_resources(
        provider: ProviderKind,
        workload: TransferWorkload,
        logical_cpus: usize,
        available_memory: Option<u64>,
    ) -> Self {
        Self::from_worker_resources(
            provider,
            workload,
            WorkerResources {
                logical_cpus,
                available_memory,
                io_pressure: false,
            },
        )
    }

    /// Resolve automatic provider ceilings from an explicit resource snapshot,
    /// including its IO-pressure signal, without probing the OS.
    ///
    /// `resources.available_memory` is raw currently available memory. Do not
    /// reserve headroom or divide it before calling: the shared model assigns
    /// a quarter per workspace/buffer class and uses 512 MiB when unknown.
    /// Provider capacity remains an independent upper bound.
    pub fn from_worker_resources(
        provider: ProviderKind,
        workload: TransferWorkload,
        resources: WorkerResources,
    ) -> Self {
        let capabilities = ProviderCapabilities::for_provider(provider);
        let resource_budget =
            resources.cpu_workers(workload, capabilities.estimated_memory_per_transfer);
        let upload_budget = resource_budget
            .min(capabilities.max_upload_concurrency)
            .max(1);
        let download_budget = resource_budget
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
        tuning.rclone_download_streams = download_streams(download_budget);
        tuning.max_bulk_bytes = resources
            .memory_budget_bytes()
            .min(
                u64::try_from(transfer_budget)
                    .unwrap_or(u64::MAX)
                    .saturating_mul(STAGED_BYTES_PER_TRANSFER),
            )
            .max(1);
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

    /// Estimate the process count when every copyto uses its full configured
    /// range-stream width. This is not an admission limit: the operation-wide
    /// object/network-slot ceiling is `max_parallel_downloads`, and each copyto
    /// must obtain weighted slots and set its stream width from the actual
    /// grant. Ordered cat streams consume one slot, not this multiplier.
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

fn download_streams(budget: usize) -> usize {
    // Preferred copyto width scales with sqrt(connection slots), without a fixed
    // app cap. Weighted permits grant any final child only the remaining slots,
    // so the width need not divide the budget. Ordered `rclone cat` streams
    // still use one connection per object.
    (1..=budget)
        .take_while(|streams| *streams <= budget / *streams)
        .last()
        .unwrap_or(1)
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
        let provider = provider.into();
        let backend_type = backend_type.into();
        let root = root.into();
        // Absolute and home-relative SFTP/local roots are different repositories.
        // Preserve the historical normalization for object-store prefixes.
        let root = if matches!(backend_type.as_str(), "sftp" | "local") {
            let root = root.trim();
            if !root.is_empty() && root.chars().all(|ch| ch == '/') {
                "/".into()
            } else {
                root.trim_end_matches('/').to_string()
            }
        } else {
            normalize_root(&root)
        };
        Self {
            provider,
            backend_type,
            remote_name: remote_name.into(),
            root,
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
            "permission denied",
            "accessdenied",
            "invalidaccesskeyid",
            "signaturedoesnotmatch",
            "expiredtoken",
            "invalid token",
            "status code: 401",
            "status code: 403",
            "http status 401",
            "http status 403",
            "http error 401",
            "http error 403",
        ],
    ) {
        return RemoteErrorClass::Authentication;
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

    // Only rclone's structured directory/file-absence exit codes establish
    // absence. "Not found" can name an executable, config section, credential,
    // DNS host or token endpoint; an arbitrary backend 404 is not proof that
    // the requested repository object is absent. Mixed auth/transport failures
    // above always win, even if an object/path contains absence-looking text.
    if matches!(exit_code, Some(3 | 4))
        && !contains_any(
            &message,
            &[
                "config file",
                "config section",
                "didn't find section",
                "executable file",
                "failed to get token",
                "failed to refresh token",
            ],
        )
    {
        return RemoteErrorClass::NotFound;
    }
    if matches!(exit_code, Some(5 | 10)) {
        return RemoteErrorClass::Retryable;
    }
    RemoteErrorClass::Permanent
}

fn contains_any(message: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| message.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_transfer_bypass_preserves_the_normal_profile_and_explicit_balanced_mode() {
        for (normal, active) in [
            (TransferWorkload::GameplaySafe, TransferWorkload::Throughput),
            (TransferWorkload::Balanced, TransferWorkload::Balanced),
            (TransferWorkload::Throughput, TransferWorkload::Throughput),
        ] {
            let saved = normal;
            assert_eq!(normal.for_active_transfer(), active);
            assert_eq!(normal, saved, "normal protection must never be mutated");
            assert_eq!(active.for_active_transfer(), active);
        }
    }

    #[test]
    fn active_transfers_bypass_gameplay_but_keep_resource_and_provider_limits() {
        let resources = WorkerResources {
            logical_cpus: 30,
            available_memory: Some(20 * 1024 * MIB),
            io_pressure: false,
        };
        let normal = TransferWorkload::GameplaySafe;
        for (provider, active_slots) in [
            (ProviderKind::AmazonS3, 30),
            (ProviderKind::GoogleDrive, 16),
            (ProviderKind::Sftp, 8),
        ] {
            let before = TransferTuning::from_worker_resources(provider, normal, resources);
            let active = TransferTuning::from_worker_resources(
                provider,
                normal.for_active_transfer(),
                resources,
            );
            assert_eq!(before.max_parallel_uploads, 8);
            assert_eq!(before.max_parallel_downloads, 8);
            assert_eq!(active.max_parallel_uploads, active_slots);
            assert_eq!(active.max_parallel_downloads, active_slots);
            assert_eq!(active.min_request_interval_ms, 0);
            assert_eq!(
                TransferTuning::from_worker_resources(provider, normal, resources),
                before,
                "normal gameplay admission returns unchanged after the operation"
            );
        }
        for (memory, pressure, expected) in [
            (Some(20 * 1024 * MIB), true, 15),
            (Some(1024 * MIB), false, 2),
            (None, false, 1),
        ] {
            let active = TransferTuning::from_worker_resources(
                ProviderKind::AmazonS3,
                normal.for_active_transfer(),
                WorkerResources {
                    available_memory: memory,
                    io_pressure: pressure,
                    ..resources
                },
            );
            assert_eq!(active.max_parallel_uploads, expected);
            assert_eq!(active.max_parallel_downloads, expected);
        }
    }

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
            32,
            Some(16 * 1024 * MIB),
        );
        assert_eq!(object.max_parallel_uploads, 32);
        assert_eq!(object.max_parallel_downloads, 32);
        assert_eq!(object.rclone_transfers, 32);
        assert_eq!(object.rclone_checkers, 32);
        assert_eq!(object.max_bulk_bytes, 4 * 1024 * MIB);

        let drive = TransferTuning::for_resources(
            ProviderKind::GoogleDrive,
            TransferWorkload::Throughput,
            64,
            Some(64 * 1024 * MIB),
        );
        assert_eq!(drive.max_parallel_uploads, 16);
        assert_eq!(drive.max_parallel_downloads, 16);
        assert_eq!(drive.rclone_transfers, 16);
        assert_eq!(drive.rclone_download_streams, 4);
        assert_eq!(drive.parallel_download_processes(), 4);
        assert_eq!(drive.max_bulk_bytes, 16 * 1024 * MIB);
    }

    #[test]
    fn automatic_budget_respects_memory_and_gameplay_pressure() {
        let memory_limited = TransferTuning::for_resources(
            ProviderKind::AmazonS3,
            TransferWorkload::Throughput,
            32,
            Some(1024 * MIB),
        );
        assert_eq!(memory_limited.max_parallel_uploads, 2);
        assert_eq!(memory_limited.max_parallel_downloads, 2);
        assert_eq!(memory_limited.max_bulk_bytes, 256 * MIB);

        let gameplay = TransferTuning::for_resources(
            ProviderKind::AmazonS3,
            TransferWorkload::GameplaySafe,
            16,
            Some(16 * 1024 * MIB),
        );
        assert_eq!(gameplay.max_parallel_uploads, 4);
        assert_eq!(gameplay.max_parallel_downloads, 4);
        assert_eq!(gameplay.rclone_download_streams, 2);
        assert_eq!(gameplay.parallel_download_processes(), 2);
        assert!(gameplay.max_parallel_uploads < 32);
    }

    #[test]
    fn automatic_limits_scale_with_cpus_then_stop_at_unchanged_provider_caps() {
        for (provider, provider_max) in [
            (ProviderKind::GoogleDrive, 16usize),
            (ProviderKind::MicrosoftOneDrive, 16),
            (ProviderKind::AmazonS3, 32),
            (ProviderKind::BackblazeB2, 32),
            (ProviderKind::CloudflareR2, 32),
            (ProviderKind::Wasabi, 32),
            (ProviderKind::DigitalOceanSpaces, 32),
            (ProviderKind::GenericS3, 32),
            (ProviderKind::GoogleCloudStorage, 32),
            (ProviderKind::AzureBlob, 32),
            (ProviderKind::Dropbox, 32),
            (ProviderKind::Box, 8),
            (ProviderKind::Sftp, 8),
            (ProviderKind::Webdav, 8),
            (ProviderKind::Local, 32),
        ] {
            for (cpus, workload, cpu_max) in [
                (8, TransferWorkload::Throughput, 8),
                (16, TransferWorkload::Throughput, 16),
                (32, TransferWorkload::Throughput, 32),
                (64, TransferWorkload::Balanced, 32),
                (128, TransferWorkload::GameplaySafe, 32),
            ] {
                let tuning =
                    TransferTuning::for_resources(provider, workload, cpus, Some(64 * 1024 * MIB));
                let expected = cpu_max.min(provider_max);
                assert_eq!(tuning.max_parallel_uploads, expected);
                assert_eq!(tuning.max_parallel_downloads, expected);
                assert_eq!(tuning.rclone_transfers, expected);
                assert_eq!(tuning.rclone_checkers, expected);
                assert_eq!(
                    tuning.parallel_download_processes(),
                    expected.div_ceil(tuning.rclone_download_streams)
                );
            }
        }
    }

    #[test]
    fn unknown_and_poor_memory_have_finite_automatic_limits() {
        for (memory, expected_bytes) in
            [(None, 128 * MIB), (Some(64 * MIB), 16 * MIB), (Some(0), 1)]
        {
            let tuning = TransferTuning::for_resources(
                ProviderKind::AmazonS3,
                TransferWorkload::Throughput,
                128,
                memory,
            );
            assert_eq!(tuning.max_parallel_uploads, 1);
            assert_eq!(tuning.max_parallel_downloads, 1);
            assert_eq!(tuning.rclone_transfers, 1);
            assert_eq!(tuning.rclone_checkers, 1);
            assert_eq!(tuning.rclone_download_streams, 1);
            assert_eq!(tuning.max_bulk_bytes, expected_bytes);
        }
    }

    #[test]
    fn automatic_io_and_gameplay_limits_compose_before_provider_caps() {
        let tuning = TransferTuning::from_worker_resources(
            ProviderKind::AmazonS3,
            TransferWorkload::GameplaySafe,
            WorkerResources {
                logical_cpus: 128,
                available_memory: Some(64 * 1024 * MIB),
                io_pressure: true,
            },
        );
        assert_eq!(tuning.max_parallel_uploads, 16);
        assert_eq!(tuning.max_parallel_downloads, 16);
        assert_eq!(tuning.rclone_transfers, 16);
        assert_eq!(tuning.rclone_checkers, 16);
        assert_eq!(tuning.max_bulk_files, 128);
        assert_eq!(tuning.max_bulk_bytes, 16 * 1024 * MIB);
        assert_eq!(tuning.min_request_interval_ms, 25);
    }

    #[test]
    fn automatic_staging_scales_with_memory_and_active_transfer_waves() {
        let tuning = |cpus, memory| {
            TransferTuning::for_resources(
                ProviderKind::AmazonS3,
                TransferWorkload::Throughput,
                cpus,
                Some(memory),
            )
        };
        assert_eq!(tuning(32, 64 * 1024 * MIB).max_bulk_bytes, 16 * 1024 * MIB);
        assert_eq!(tuning(32, 128 * 1024 * MIB).max_bulk_bytes, 32 * 1024 * MIB);
        // Very large memory does not stage indefinitely behind one slow slot.
        assert_eq!(tuning(1, 128 * 1024 * MIB).max_bulk_bytes, 2 * 1024 * MIB);
        assert_eq!(tuning(2, 128 * 1024 * MIB).max_bulk_bytes, 4 * 1024 * MIB);
    }

    #[test]
    fn automatic_extreme_inputs_are_finite_and_do_not_overflow() {
        for workload in [
            TransferWorkload::Throughput,
            TransferWorkload::Balanced,
            TransferWorkload::GameplaySafe,
        ] {
            let tuning = TransferTuning::for_resources(
                ProviderKind::AmazonS3,
                workload,
                usize::MAX,
                Some(u64::MAX),
            );
            assert_eq!(tuning.max_parallel_uploads, 32);
            assert_eq!(tuning.max_parallel_downloads, 32);
            assert_eq!(tuning.max_bulk_bytes, 64 * 1024 * MIB);
            assert_eq!(tuning.rclone_download_streams, 5);
            assert_eq!(tuning.parallel_download_processes(), 7);
            assert_eq!(tuning.max_attempts, TransferTuning::default().max_attempts);
            assert_eq!(
                tuning.max_bulk_files,
                match workload {
                    TransferWorkload::Throughput => 1024,
                    TransferWorkload::Balanced => 256,
                    TransferWorkload::GameplaySafe => 128,
                }
            );
        }
    }

    #[test]
    fn automatic_stream_grouping_tracks_square_root_without_a_fixed_cap() {
        for budget in 1..=128 {
            let streams = download_streams(budget);
            assert!(streams >= 1);
            assert!(streams <= budget / streams);
            let next = streams + 1;
            assert!(next > budget / next);
            let tuning = TransferTuning {
                max_parallel_downloads: budget,
                rclone_download_streams: streams,
                ..TransferTuning::default()
            };
            assert_eq!(
                tuning.parallel_download_processes(),
                budget.div_ceil(streams)
            );
        }
    }

    #[test]
    fn prime_download_budget_keeps_multistream_preference_and_a_partial_final_grant() {
        let tuning = TransferTuning::for_resources(
            ProviderKind::AmazonS3,
            TransferWorkload::Throughput,
            19,
            Some(64 * 1024 * MIB),
        );
        assert_eq!(tuning.max_parallel_downloads, 19);
        assert_eq!(tuning.rclone_download_streams, 4);
        assert_eq!(tuning.parallel_download_processes(), 5);
        // The backend's weighted admission, not this ceil process estimate,
        // bounds actual connections. Full-width children leave three slots.
        let full_width_children = tuning.max_parallel_downloads / tuning.rclone_download_streams;
        let final_width = tuning.max_parallel_downloads % tuning.rclone_download_streams;
        assert_eq!((full_width_children, final_width), (4, 3));
    }

    #[test]
    fn explicit_tuning_profiles_keep_their_serialization_defaults() {
        assert_eq!(
            serde_json::from_str::<TransferTuning>("{}").unwrap(),
            TransferTuning::default()
        );
        for tuning in [
            TransferTuning::default(),
            TransferTuning::throughput(),
            TransferTuning::gameplay_safe(),
        ] {
            let wire = serde_json::to_string(&tuning).unwrap();
            assert_eq!(
                serde_json::from_str::<TransferTuning>(&wire).unwrap(),
                tuning
            );
            assert!(!wire.contains("available_memory"));
            assert!(!wire.contains("logical_cpus"));
            assert!(!wire.contains("io_pressure"));
        }
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

    #[test]
    fn absence_requires_structured_rclone_status_and_never_masks_auth_or_transport() {
        for code in [3, 4] {
            assert_eq!(
                classify_remote_error(Some(code), ""),
                RemoteErrorClass::NotFound
            );
            assert_eq!(
                classify_remote_error(Some(code), "object not found"),
                RemoteErrorClass::NotFound
            );
            for error in [
                "object not found: 403 Forbidden",
                "object not found: permission denied",
                "NoSuchKey: AccessDenied",
                "object not found: InvalidAccessKeyId",
                "object not found: SignatureDoesNotMatch",
                "object not found: ExpiredToken",
            ] {
                assert_eq!(
                    classify_remote_error(Some(code), error),
                    RemoteErrorClass::Authentication
                );
            }
            assert_eq!(
                classify_remote_error(Some(code), "object not found: connection reset by peer"),
                RemoteErrorClass::Retryable
            );
        }
        for error in [
            "executable file not found",
            "config file not found",
            "config section not found",
            "object not found",
            "status code: 404",
            "token endpoint not found",
        ] {
            assert_ne!(
                classify_remote_error(Some(1), error),
                RemoteErrorClass::NotFound
            );
            assert_ne!(
                classify_remote_error(None, error),
                RemoteErrorClass::NotFound
            );
        }
        assert_eq!(
            classify_remote_error(Some(5), "object not found"),
            RemoteErrorClass::Retryable
        );
        assert_ne!(
            classify_remote_error(Some(9), ""),
            RemoteErrorClass::NotFound
        );
    }
}
#[test]
fn filesystem_repository_identity_preserves_absolute_roots() {
    for backend in ["sftp", "local"] {
        let absolute = ProviderRootIdentity::new(backend, backend, "store", "/data");
        let relative = ProviderRootIdentity::new(backend, backend, "store", "data");
        assert_ne!(absolute.cache_key(), relative.cache_key());
        assert_ne!(
            ProviderRootIdentity::new(backend, backend, "store", "/").cache_key(),
            ProviderRootIdentity::new(backend, backend, "store", "").cache_key()
        );
    }
}
