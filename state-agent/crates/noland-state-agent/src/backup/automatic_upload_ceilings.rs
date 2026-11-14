//! Controlled policy tests: no environment mutation or host-performance assumptions.
use super::*;
use noland_rclone_adapter::{ProviderCapabilities, WorkerResources};

#[test]
fn active_upload_profiles_preserve_balanced_and_ignore_gameplay_without_mutation() {
    let resources = WorkerResources {
        logical_cpus: 30,
        available_memory: Some(20 << 30),
        io_pressure: false,
    };
    for mode in [
        BackupPerformanceMode::Automatic,
        BackupPerformanceMode::Balanced,
        BackupPerformanceMode::Fast,
        BackupPerformanceMode::Full,
    ] {
        let expected_workload = if mode == BackupPerformanceMode::Balanced {
            TransferWorkload::Balanced
        } else {
            TransferWorkload::Throughput
        };
        for gameplay in [false, true] {
            let (baseline, effective) = backup_transfer_workloads(mode, gameplay);
            assert_eq!(effective, expected_workload);
            assert_eq!(
                baseline,
                if gameplay {
                    TransferWorkload::GameplaySafe
                } else {
                    expected_workload
                }
            );
            for provider in [ProviderKind::AmazonS3, ProviderKind::GoogleDrive] {
                let admitted = upload_tuning_for_resources(provider, effective, resources, None);
                let expected = if mode == BackupPerformanceMode::Balanced {
                    15
                } else if provider == ProviderKind::GoogleDrive {
                    16
                } else {
                    30
                };
                assert_eq!(admitted.max_parallel_uploads, expected);
                assert_eq!(admitted.max_parallel_downloads, expected);
                assert_eq!(admitted.rclone_transfers, expected);
                // The baseline remains available to non-operation protection;
                // neither successful nor abandoned budget calculation mutates it.
                assert_eq!(backup_transfer_workloads(mode, gameplay).0, baseline);
            }
        }
    }
}

#[test]
fn automatic_upload_ceiling_exceeds_eight_without_losing_provider_limits() {
    for cpus in [32, 64] {
        for workload in [TransferWorkload::Throughput, TransferWorkload::Balanced] {
            for provider in [ProviderKind::Local, ProviderKind::GoogleDrive] {
                let resources = WorkerResources {
                    logical_cpus: cpus,
                    available_memory: Some(64 << 30),
                    io_pressure: false,
                };
                let resolved = TransferTuning::from_worker_resources(provider, workload, resources);
                let admitted = upload_tuning_for_resources(provider, workload, resources, None);
                assert!(admitted.max_parallel_uploads > 8);
                assert_eq!(admitted.max_parallel_uploads, resolved.max_parallel_uploads);
                assert_eq!(admitted.rclone_transfers, admitted.max_parallel_uploads);
                assert!(
                    admitted.max_parallel_uploads
                        <= ProviderCapabilities::for_provider(provider).max_upload_concurrency
                );
            }
        }
    }
}

#[test]
fn automatic_upload_overrides_only_lower_the_resolved_ceiling() {
    for workload in [
        TransferWorkload::Throughput,
        TransferWorkload::Balanced,
        TransferWorkload::GameplaySafe,
    ] {
        for memory in [Some(0), Some(192 << 20), Some(64 << 30), None] {
            for io_pressure in [false, true] {
                let resources = WorkerResources {
                    logical_cpus: 64,
                    available_memory: memory,
                    io_pressure,
                };
                let resolved = TransferTuning::from_worker_resources(
                    ProviderKind::Local,
                    workload.for_active_transfer(),
                    resources,
                );
                for requested in [0, 1, 3, 8, usize::MAX] {
                    let admitted = upload_tuning_for_resources(
                        ProviderKind::Local,
                        workload,
                        resources,
                        Some(requested),
                    );
                    assert_eq!(
                        admitted.max_parallel_uploads,
                        requested.clamp(1, resolved.max_parallel_uploads)
                    );
                    assert_eq!(admitted.rclone_transfers, admitted.max_parallel_uploads);
                    assert_eq!(
                        admitted.max_parallel_downloads,
                        resolved.max_parallel_downloads
                    );
                    assert_eq!(admitted.max_bulk_bytes, resolved.max_bulk_bytes);
                }
            }
        }
    }
}

#[test]
fn automatic_upload_uses_raw_memory_without_a_second_headroom_reservation() {
    // The shared resolver applies its documented reservation once. Backup must
    // not halve this raw memory again or impose another local eight-slot limit.
    let resources = WorkerResources {
        logical_cpus: 64,
        available_memory: Some(2 << 30),
        io_pressure: false,
    };
    let resolved = TransferTuning::from_worker_resources(
        ProviderKind::Local,
        TransferWorkload::Throughput,
        resources,
    );
    let admitted = upload_tuning_for_resources(
        ProviderKind::Local,
        TransferWorkload::Throughput,
        resources,
        Some(usize::MAX),
    );
    assert!(admitted.max_parallel_uploads > 8);
    assert_eq!(admitted, resolved);
}

#[test]
fn automatic_upload_resource_helper_preserves_injected_io_pressure() {
    for workload in [
        TransferWorkload::Throughput,
        TransferWorkload::Balanced,
        TransferWorkload::GameplaySafe,
    ] {
        for memory in [Some(192 << 20), Some(2 << 30), Some(64 << 30), None] {
            let resources = WorkerResources {
                logical_cpus: 32,
                available_memory: memory,
                io_pressure: true,
            };
            let provider = ProviderKind::Local;
            let capabilities = ProviderCapabilities::for_provider(provider);
            let expected = resources
                .cpu_workers(
                    workload.for_active_transfer(),
                    capabilities.estimated_memory_per_transfer,
                )
                .min(capabilities.max_upload_concurrency);
            let admitted =
                upload_tuning_for_resources(provider, workload, resources, Some(usize::MAX));
            assert_eq!(admitted.max_parallel_uploads, expected);
            assert_eq!(admitted.rclone_transfers, expected);
            assert_eq!(
                admitted,
                TransferTuning::from_worker_resources(
                    provider,
                    workload.for_active_transfer(),
                    resources,
                )
            );
        }
    }
}
