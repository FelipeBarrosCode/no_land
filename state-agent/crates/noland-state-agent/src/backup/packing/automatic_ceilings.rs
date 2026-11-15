use super::*;
use noland_rclone_adapter::WorkerResources;

fn budget(
    cpus: usize,
    memory: Option<u64>,
    gameplay: bool,
    io_pressure: bool,
) -> preparation::Budget {
    preparation::Budget::for_worker_resources(
        WorkerResources {
            logical_cpus: cpus,
            available_memory: memory,
            io_pressure,
        },
        BackupPerformanceMode::Automatic,
        gameplay,
    )
}

#[test]
fn balanced_legacy_packer_replay_intersects_saved_counts_with_current_profile() {
    let current = |cpus, gameplay, io_pressure| {
        preparation::Budget::for_worker_resources(
            WorkerResources {
                logical_cpus: cpus,
                available_memory: Some(64 << 30),
                io_pressure,
            },
            BackupPerformanceMode::Balanced,
            gameplay,
        )
    };
    let saved = current(128, false, false).packing;
    assert_eq!(saved, 32);
    for (gameplay, io_pressure, expected) in [
        (false, false, 16),
        (false, true, 8),
        (true, false, 16),
        (true, true, 8),
    ] {
        let admitted = current(64, gameplay, io_pressure);
        assert_eq!(admitted_pack_workers(saved, admitted, 64), expected);
        assert_eq!(admitted_pack_workers(2, admitted, 64), 2);
        assert_eq!(admitted_pack_workers(saved, admitted, 1), 1);
        assert_eq!(admitted_pack_workers(saved, admitted, 0), 0);
    }
}

#[test]
fn automatic_legacy_packing_ceiling_scales_above_thirty_two_and_respects_frozen_lowering() {
    for cpus in [32, 64, 128] {
        let current = budget(cpus, Some(64 << 30), false, false);
        let count = admitted_pack_workers(usize::MAX, current, 256);
        assert_eq!(count, current.packing);
        assert!(count > 8);
        if cpus == 128 {
            assert!(count > 32);
        }
        assert_eq!(admitted_pack_workers(1, current, 256), 1);
        assert_eq!(admitted_pack_workers(3, current, 256), 3);
        assert_eq!(admitted_pack_workers(usize::MAX, current, 2), 2);
        assert_eq!(admitted_pack_workers(usize::MAX, current, 0), 0);
    }
}

#[test]
fn automatic_legacy_packing_ceiling_cannot_bypass_memory_or_pressure() {
    for gameplay in [false, true] {
        for io_pressure in [false, true] {
            for memory in [Some(0), Some(192 << 20), Some(64 << 30), None] {
                let current = budget(64, memory, gameplay, io_pressure);
                let count = admitted_pack_workers(usize::MAX, current, usize::MAX);
                assert_eq!(count, current.packing);
                assert!((1..=32).contains(&count));
                assert_eq!(admitted_pack_workers(0, current, 100), 1);
            }
        }
    }
    let low = budget(64, Some(192 << 20), true, true);
    assert_eq!(admitted_pack_workers(usize::MAX, low, 100), 1);
}
