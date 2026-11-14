//! One CPU/memory/IO budget shared by overlapping capture and compression.
use super::*;
use noland_rclone_adapter::WorkerResources;

const MIN_WORKERS: usize = 2;
// Covers each reader's chunk/replay buffers or each codec's verified plaintext,
// compression scratch/context and ciphertext, rather than just one buffer.
// Capture receipts queue disk-backed ChunkSources, not SEGMENT_BYTES payloads;
// their bounded metadata and coordinator overhead fit in the policy's headroom.
pub(super) const MEMORY_PER_WORKER: u64 = 96 * 1024 * 1024;

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub(super) struct Budget {
    pub hashing: usize,
    pub packing: usize,
}

impl Budget {
    pub fn detect(agent: &StateAgent, mode: BackupPerformanceMode) -> Result<Self> {
        let gameplay = agent.has_latency_sensitive_sessions()?;
        let resources = WorkerResources::detect();
        let requested = |name| {
            std::env::var(name)
                .ok()
                .and_then(|value| value.parse().ok())
        };
        Ok(
            Self::for_worker_resources(resources, mode, gameplay).with_worker_limits(
                requested("NOLAND_HASH_WORKERS"),
                requested("NOLAND_PACK_WORKERS"),
            ),
        )
    }

    /// Saved counts freeze only an upper bound, never a resource entitlement.
    /// Preserve explicit benchmark lowering when admitting a replay on a new host.
    pub fn admitted(self, current: Self) -> Self {
        current.with_worker_limits(Some(self.hashing), Some(self.packing))
    }

    fn with_worker_limits(mut self, hashing: Option<usize>, packing: Option<usize>) -> Self {
        // Overrides only lower each stage's admission; they cannot borrow slots
        // from the other stage or defeat memory or disk-pressure caps.
        if let Some(requested) = hashing {
            self.hashing = requested.clamp(1, self.hashing);
        }
        if let Some(requested) = packing {
            self.packing = requested.clamp(1, self.packing);
        }
        self
    }

    #[cfg(test)]
    fn for_resources(
        cpus: usize,
        memory: Option<u64>,
        mode: BackupPerformanceMode,
        gameplay: bool,
        io_pressure: bool,
    ) -> Self {
        Self::for_worker_resources(
            WorkerResources {
                logical_cpus: cpus,
                available_memory: memory,
                io_pressure,
            },
            mode,
            gameplay,
        )
    }

    pub fn for_worker_resources(
        resources: WorkerResources,
        mode: BackupPerformanceMode,
        gameplay: bool,
    ) -> Self {
        let (_, workload) = backup_transfer_workloads(mode, gameplay);
        // Pass raw available memory: shared policy reserves headroom once.
        // Only our worker admission changes; never application CPU/GPU quotas,
        // affinity or cgroups. One reader plus one codec is the liveness floor.
        let total = resources
            .cpu_workers(workload, MEMORY_PER_WORKER)
            .max(MIN_WORKERS);
        let hashing = total.div_ceil(2);
        Self {
            hashing,
            packing: total - hashing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const MODES: [BackupPerformanceMode; 4] = [
        BackupPerformanceMode::Automatic,
        BackupPerformanceMode::Balanced,
        BackupPerformanceMode::Fast,
        BackupPerformanceMode::Full,
    ];

    #[test]
    fn active_transfer_admission_is_identical_with_or_without_gameplay_in_every_mode() {
        for mode in MODES {
            for cpus in [0usize, 1, 2, 4, 8, 9, 19, 30, 32, 48, 128] {
                let expected = if mode == BackupPerformanceMode::Balanced {
                    cpus.div_ceil(2)
                } else {
                    cpus
                }
                .max(MIN_WORKERS);
                for gameplay in [false, true] {
                    let budget = Budget::for_resources(cpus, Some(64 << 30), mode, gameplay, false);
                    assert_eq!(budget.hashing + budget.packing, expected, "{cpus} CPUs");
                    assert_eq!(budget.hashing, expected.div_ceil(2));
                    assert!(budget.packing >= 1);
                }
            }
        }
        let budget = Budget::for_resources(
            30,
            Some(20 << 30),
            BackupPerformanceMode::Automatic,
            true,
            false,
        );
        assert_eq!((budget.hashing, budget.packing), (15, 15));
        let balanced = Budget::for_resources(
            30,
            Some(20 << 30),
            BackupPerformanceMode::Balanced,
            true,
            false,
        );
        assert_eq!((balanced.hashing, balanced.packing), (8, 7));
    }

    #[test]
    fn memory_and_disk_pressure_limit_both_gameplay_and_idle_admission() {
        for mode in MODES {
            for gameplay in [false, true] {
                for memory in [
                    None,
                    Some(0),
                    Some(384 << 20),
                    Some(768 << 20),
                    Some(1 << 30),
                    Some(18 << 30),
                    Some(u64::MAX),
                ] {
                    let budget = Budget::for_resources(48, memory, mode, gameplay, false);
                    let workload = if mode == BackupPerformanceMode::Balanced {
                        TransferWorkload::Balanced
                    } else {
                        TransferWorkload::Throughput
                    };
                    let expected = WorkerResources {
                        logical_cpus: 48,
                        available_memory: memory,
                        io_pressure: false,
                    }
                    .cpu_workers(workload, MEMORY_PER_WORKER)
                    .max(MIN_WORKERS);
                    assert_eq!(budget.hashing + budget.packing, expected);
                    let pressured = Budget::for_resources(48, memory, mode, gameplay, true);
                    assert!(pressured.hashing + pressured.packing <= expected);
                    assert!(pressured.hashing >= 1 && pressured.packing >= 1);
                }
                let low_cpu = Budget::for_resources(2, Some(18 << 30), mode, gameplay, true);
                assert_eq!((low_cpu.hashing, low_cpu.packing), (1, 1));
            }
        }
    }

    #[test]
    fn automatic_ceiling_uses_half_balanced_cpus_and_a_high_emergency_guard() {
        let balanced = Budget::for_resources(
            9,
            Some(18 << 30),
            BackupPerformanceMode::Balanced,
            false,
            false,
        );
        assert_eq!((balanced.hashing, balanced.packing), (3, 2));
        let fast =
            Budget::for_resources(9, Some(18 << 30), BackupPerformanceMode::Fast, false, false);
        assert_eq!((fast.hashing, fast.packing), (5, 4));
        for mode in MODES {
            let budget = Budget::for_resources(usize::MAX, Some(u64::MAX), mode, false, false);
            let total = budget.hashing + budget.packing;
            assert!(total > 12 && total < usize::MAX / 2);
        }
    }

    #[test]
    fn automatic_ceiling_matrix_admits_large_hosts_and_keeps_one_shared_total() {
        for cpus in [32, 64] {
            for mode in MODES {
                let budget = Budget::for_resources(cpus, Some(64 << 30), mode, false, false);
                let expected = if mode == BackupPerformanceMode::Balanced {
                    cpus / 2
                } else {
                    cpus
                };
                assert_eq!(budget.hashing + budget.packing, expected);
                assert!(budget.hashing > 4);
                assert!(budget.hashing + budget.packing > 12);

                let gameplay = Budget::for_resources(cpus, Some(64 << 30), mode, true, false);
                assert_eq!(gameplay.hashing + gameplay.packing, expected);
                let pressure = Budget::for_resources(cpus, Some(64 << 30), mode, false, true);
                assert!(pressure.hashing + pressure.packing < expected);
                // IO pressure scales down instead of pinning every host to four.
                if cpus == 64 {
                    assert!(pressure.hashing + pressure.packing > 4);
                }
                let low = Budget::for_resources(cpus, Some(192 << 20), mode, false, false);
                assert_eq!((low.hashing, low.packing), (1, 1));
            }
        }
    }

    #[test]
    fn automatic_ceiling_replay_preserves_saved_lowering_and_bounds_corrupt_counts() {
        let current = Budget::for_resources(
            64,
            Some(64 << 30),
            BackupPerformanceMode::Automatic,
            false,
            false,
        );
        let lowered = Budget {
            hashing: 1,
            packing: 2,
        }
        .admitted(current);
        assert_eq!((lowered.hashing, lowered.packing), (1, 2));
        let oversized = Budget {
            hashing: usize::MAX,
            packing: usize::MAX,
        }
        .admitted(current);
        assert_eq!(oversized.hashing, current.hashing);
        assert_eq!(oversized.packing, current.packing);
        let low = oversized.admitted(Budget::for_resources(
            64,
            Some(192 << 20),
            BackupPerformanceMode::Automatic,
            true,
            true,
        ));
        assert_eq!((low.hashing, low.packing), (1, 1));
    }

    #[test]
    fn explicit_worker_limits_can_only_lower_each_stage() {
        for (gameplay, io_pressure, memory) in [
            (false, false, Some(18 << 30)),
            (true, false, Some(18 << 30)),
            (true, true, Some(18 << 30)),
            (true, true, Some(384 << 20)),
        ] {
            let admitted = Budget::for_resources(
                19,
                memory,
                BackupPerformanceMode::Full,
                gameplay,
                io_pressure,
            );
            let unchanged = admitted.with_worker_limits(Some(usize::MAX), Some(usize::MAX));
            assert_eq!(unchanged.hashing, admitted.hashing);
            assert_eq!(unchanged.packing, admitted.packing);
            let lowered = admitted.with_worker_limits(Some(0), Some(1));
            assert_eq!((lowered.hashing, lowered.packing), (1, 1));
            let one_stage = admitted.with_worker_limits(Some(1), None);
            assert_eq!(one_stage.hashing, 1);
            assert_eq!(one_stage.packing, admitted.packing);
            let packing_only = admitted.with_worker_limits(None, Some(1));
            assert_eq!(packing_only.hashing, admitted.hashing);
            assert_eq!(packing_only.packing, 1);
            let no_limits = admitted.with_worker_limits(None, None);
            assert_eq!(no_limits.hashing, admitted.hashing);
            assert_eq!(no_limits.packing, admitted.packing);
        }
    }

    #[test]
    fn serialized_budget_keeps_only_the_existing_worker_counts() {
        let budget = Budget::for_resources(
            19,
            Some(18 << 30),
            BackupPerformanceMode::Automatic,
            true,
            false,
        );
        assert_eq!(
            serde_json::to_value(budget).unwrap(),
            serde_json::json!({ "hashing": 10, "packing": 9 })
        );
    }
}
