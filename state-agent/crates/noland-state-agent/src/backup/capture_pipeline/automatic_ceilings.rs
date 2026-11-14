//! Legacy replay admission tests use explicit resources, never host CPU counts.
use super::*;
use noland_rclone_adapter::WorkerResources;

fn current_budget(
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

fn saved_state(jobs: &[(HashJob, Identity)], hashing: usize, packing: usize) -> State {
    State {
        streaming: false,
        jobs: jobs.to_vec(),
        ranges: (0..jobs.len()).map(|index| (index, index + 1)).collect(),
        snapshot: None,
        budget: preparation::Budget { hashing, packing },
    }
}

#[test]
fn balanced_replay_host_shrink_keeps_current_profile_for_v2_and_v3() {
    let (root, agent, _, _, prepared) = tests::fixture(64);
    let jobs = &prepared.capture.as_ref().unwrap().jobs;
    let budget = |cpus, gameplay, io_pressure| {
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
    let saved = budget(128, false, false);
    assert_eq!((saved.hashing, saved.packing), (32, 32));
    for streaming in [false, true] {
        let mut frozen = saved_state(jobs, saved.hashing, saved.packing);
        frozen.streaming = streaming;
        let bytes = serde_json::to_vec(&frozen).unwrap();
        let frozen_jobs = serde_json::to_value(&frozen.jobs).unwrap();
        for (gameplay, io_pressure, expected) in [
            (false, false, 16),
            (false, true, 8),
            (true, false, 16),
            (true, true, 8),
        ] {
            let current = budget(64, gameplay, io_pressure);
            let replay: State = serde_json::from_slice(&bytes).unwrap();
            let admitted = replay.admitted(current).unwrap();
            assert_eq!(
                (admitted.budget.hashing, admitted.budget.packing),
                (expected, expected),
                "streaming={streaming}, gameplay={gameplay}, pressure={io_pressure}"
            );
            assert_eq!(
                hash_pool_limits(saved.hashing, jobs.len(), current).unwrap(),
                (expected, expected * 2)
            );
            assert_eq!(admitted.ranges, frozen.ranges);
            assert_eq!(admitted.streaming, frozen.streaming);
            assert_eq!(serde_json::to_value(&admitted.jobs).unwrap(), frozen_jobs);
        }
        // The current environment may lower either stage further. Neither a
        // frozen count nor the other stage's unused share may undo that limit.
        let lowered = preparation::Budget {
            hashing: 3,
            packing: 2,
        }
        .admitted(budget(64, false, false));
        let replay: State = serde_json::from_slice(&bytes).unwrap();
        let admitted = replay.admitted(lowered).unwrap();
        assert_eq!((admitted.budget.hashing, admitted.budget.packing), (3, 2));
        assert_eq!(serde_json::to_vec(&frozen).unwrap(), bytes);
    }
    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn automatic_legacy_replay_accepts_frozen_counts_above_thirty_two_without_replanning() {
    let (root, agent, _, _, prepared) = tests::fixture(3);
    let jobs = &prepared.capture.as_ref().unwrap().jobs;
    let frozen = saved_state(jobs, 64, 64);
    let bytes = serde_json::to_vec(&frozen).unwrap();
    let replay: State = serde_json::from_slice(&bytes).unwrap();
    replay.validate().unwrap();
    let admitted = replay
        .admitted(current_budget(128, Some(64 << 30), false, false))
        .unwrap();
    assert_eq!((admitted.budget.hashing, admitted.budget.packing), (64, 64));
    assert_eq!(serde_json::to_vec(&admitted).unwrap(), bytes);

    let replay: State = serde_json::from_slice(&bytes).unwrap();
    let admitted = replay
        .admitted(current_budget(8, Some(64 << 30), false, false))
        .unwrap();
    assert_eq!((admitted.budget.hashing, admitted.budget.packing), (4, 4));
    assert_eq!(admitted.ranges, frozen.ranges);
    assert_eq!(
        serde_json::to_value(&admitted.jobs).unwrap(),
        serde_json::to_value(&frozen.jobs).unwrap()
    );
    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn automatic_legacy_replay_rejects_zero_and_invalid_ranges_but_readmits_extreme_counts() {
    let (root, agent, _, _, prepared) = tests::fixture(3);
    let jobs = &prepared.capture.as_ref().unwrap().jobs;
    let current = current_budget(64, Some(64 << 30), false, false);
    for (hashing, packing) in [(0, 1), (1, 0), (0, 0)] {
        assert!(saved_state(jobs, hashing, packing).validate().is_err());
        assert!(saved_state(jobs, hashing, packing)
            .admitted(current)
            .is_err());
    }
    let mut invalid = saved_state(jobs, usize::MAX, usize::MAX);
    invalid.ranges[0] = (usize::MAX, usize::MAX);
    assert!(invalid.admitted(current).is_err());

    for gameplay in [false, true] {
        for io_pressure in [false, true] {
            let current = current_budget(64, Some(192 << 20), gameplay, io_pressure);
            let admitted = saved_state(jobs, usize::MAX, usize::MAX)
                .admitted(current)
                .unwrap();
            assert_eq!((admitted.budget.hashing, admitted.budget.packing), (1, 1));
            let lowered = saved_state(jobs, 1, 2)
                .admitted(current_budget(128, Some(64 << 30), gameplay, io_pressure))
                .unwrap();
            assert_eq!((lowered.budget.hashing, lowered.budget.packing), (1, 2));
        }
    }
    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn automatic_hash_pool_limits_share_admission_and_bound_jobs_before_channel_allocation() {
    for cpus in [32, 64, 128, usize::MAX] {
        for gameplay in [false, true] {
            for io_pressure in [false, true] {
                for memory in [None, Some(0), Some(192 << 20), Some(64 << 30)] {
                    let current = current_budget(cpus, memory, gameplay, io_pressure);
                    let (workers, capacity) =
                        hash_pool_limits(usize::MAX, usize::MAX, current).unwrap();
                    assert_eq!(workers, current.hashing);
                    assert_eq!(capacity, workers.checked_mul(2).unwrap());
                    assert!(
                        workers + current.packing <= current.hashing + current.packing,
                        "readers cannot allocate another full preparation allowance"
                    );
                    assert_eq!(hash_pool_limits(usize::MAX, 1, current).unwrap(), (1, 2));
                    assert_eq!(hash_pool_limits(usize::MAX, 0, current).unwrap(), (0, 0));
                    assert_eq!(hash_pool_limits(1, 100, current).unwrap(), (1, 2));
                }
            }
        }
    }
}

#[test]
fn automatic_hash_pool_limits_reject_zero_and_overflow_even_with_forged_current_admission() {
    let current = current_budget(128, Some(64 << 30), false, false);
    assert!(hash_pool_limits(0, 1, current).is_err());
    assert!(hash_pool_limits(0, 0, current).is_err());
    for (hashing, packing) in [(0, 1), (1, 0)] {
        assert!(hash_pool_limits(1, 1, preparation::Budget { hashing, packing }).is_err());
    }
    let forged = preparation::Budget {
        hashing: usize::MAX,
        packing: 1,
    };
    assert!(hash_pool_limits(usize::MAX, usize::MAX, forged).is_err());
    assert_eq!(hash_pool_limits(usize::MAX, 1, forged).unwrap(), (1, 2));
}

#[test]
fn automatic_hash_helper_completes_one_actual_job_with_an_extreme_saved_count() {
    let (root, agent, _, _, mut prepared) = tests::fixture(1);
    let jobs = prepared
        .capture
        .take()
        .unwrap()
        .jobs
        .into_iter()
        .map(|(job, _)| job)
        .collect();
    let cas = LocalCas::new(agent.config.paths.cache.join("cas/chunks")).unwrap();
    let lease = Arc::new(cas.lease().unwrap());
    let mut completed = 0;
    hash_files_parallel(
        jobs,
        usize::MAX,
        current_budget(8, Some(8 << 30), false, false),
        lease.clone(),
        Default::default(),
        RemoteChunkIndex::empty(),
        Utc::now(),
        Arc::new(|| false),
        |_| {
            completed += 1;
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(completed, 1);
    assert_eq!(Arc::strong_count(&lease), 1, "all hash workers joined");
    drop(lease);
    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}
