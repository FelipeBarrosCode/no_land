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
fn active_upload_with_a_game_uses_full_operation_admission_not_the_gameplay_quarter() {
    let resources = WorkerResources {
        logical_cpus: 30,
        available_memory: Some(20 << 30),
        io_pressure: false,
    };
    assert_eq!(
        resources.cpu_workers(
            TransferWorkload::GameplaySafe,
            preparation::MEMORY_PER_WORKER
        ),
        8
    );
    let current = preparation::Budget::for_worker_resources(
        resources,
        BackupPerformanceMode::Automatic,
        true,
    );
    let tuning = TransferTuning::from_worker_resources(
        ProviderKind::AmazonS3,
        TransferWorkload::GameplaySafe.for_active_transfer(),
        resources,
    );
    let slots = upload_slots(&tuning);
    let limits = Limits::new(slots, tuning.max_bulk_bytes, PACK_BYTES);
    let ceiling = WorkerCeilings::for_work(current, 100, 100, limits.capacity);
    assert_eq!((ceiling.readers, ceiling.codecs, slots), (15, 15, 30));
    assert_eq!(
        resources.cpu_workers(
            TransferWorkload::GameplaySafe,
            preparation::MEMORY_PER_WORKER
        ),
        8,
        "operation admission must not disable normal gameplay protection"
    );
}

#[test]
fn automatic_streaming_ceilings_exceed_old_reader_preparation_and_upload_caps() {
    for cpus in [32, 64] {
        let current = budget(cpus, Some(64 << 30), false, false);
        let tuning = TransferTuning::for_resources(
            ProviderKind::Local,
            TransferWorkload::Throughput,
            cpus,
            Some(64 << 30),
        );
        let slots = upload_slots(&tuning);
        let limits = Limits::new(slots, tuning.max_bulk_bytes, PACK_BYTES);
        let ceiling = WorkerCeilings::for_work(current, 128, 128, limits.capacity);
        assert!(ceiling.readers > 4);
        assert!(ceiling.readers + ceiling.codecs > 12);
        assert!(slots > 8 && slots <= 32);
        assert_eq!(ceiling.readers, current.hashing);
        assert_eq!(ceiling.codecs, current.packing);
        assert_eq!(ceiling.capture_queue, ceiling.readers * 2);
    }
}

#[test]
fn automatic_streaming_ceilings_bound_real_work_queues_and_saved_counts() {
    let current = budget(64, Some(64 << 30), false, false);
    let saved = preparation::Budget {
        hashing: usize::MAX,
        packing: usize::MAX,
    }
    .admitted(current);
    let one = WorkerCeilings::for_work(saved, 1, 1, 64);
    assert_eq!((one.readers, one.codecs, one.capture_queue), (1, 1, 2));
    let empty = WorkerCeilings::for_work(saved, 0, 0, 64);
    assert_eq!(
        (empty.readers, empty.codecs, empty.capture_queue),
        (0, 0, 1)
    );
    let small_queue = WorkerCeilings::for_work(saved, 100, 100, 2);
    assert_eq!(small_queue.codecs, 2);
    let lowered = preparation::Budget {
        hashing: 2,
        packing: 1,
    }
    .admitted(current);
    let ceiling = WorkerCeilings::for_work(lowered, 100, 100, 64);
    assert_eq!(
        (ceiling.readers, ceiling.codecs, ceiling.capture_queue),
        (2, 1, 4)
    );

    for gameplay in [false, true] {
        for io_pressure in [false, true] {
            let low = saved.admitted(budget(64, Some(192 << 20), gameplay, io_pressure));
            let ceiling = WorkerCeilings::for_work(low, usize::MAX, usize::MAX, 64);
            assert_eq!((ceiling.readers, ceiling.codecs), (1, 1));
            assert_eq!(ceiling.capture_queue, 2);
        }
    }
}

#[test]
fn automatic_streaming_codec_job_bound_includes_short_files_and_frozen_plans() {
    assert_eq!(codec_job_bound([0, 0].into_iter(), 0), 0);
    assert_eq!(codec_job_bound([1, 1].into_iter(), 3), 5);
    assert_eq!(
        codec_job_bound([constants::FASTCDC_MIN + 1].into_iter(), 0),
        2
    );
    assert_eq!(
        codec_job_bound([u64::MAX].into_iter(), usize::MAX),
        usize::MAX
    );
}

#[test]
fn automatic_streaming_upload_window_preserves_bulk_and_global_safety_limits() {
    let mut tuning = TransferTuning {
        max_parallel_uploads: usize::MAX,
        max_bulk_files: usize::MAX,
        ..Default::default()
    };
    assert_eq!(upload_slots(&tuning), 32);
    tuning.max_bulk_files = 11;
    assert_eq!(upload_slots(&tuning), 11);
    tuning.max_parallel_uploads = 3;
    assert_eq!(upload_slots(&tuning), 3);
}

#[test]
fn automatic_streaming_pools_use_admission_with_enough_actual_jobs() {
    let (root, agent, master, operation, mut prepared) = super::super::tests::fixture(64);
    let mut state = prepared.capture.take().unwrap();
    state.streaming = true;
    // Each worker has independent real source work, not a single coalesced batch.
    state.ranges = (0..state.jobs.len())
        .map(|index| (index, index + 1))
        .collect();
    state.budget = budget(32, Some(64 << 30), false, false);
    let ceilings = WorkerCeilings::for_work(state.budget, state.ranges.len(), 64, 64);
    let directory = prepared_backup_path(&agent, operation).with_extension("stream");
    let counters = Arc::new(Counters::default());
    let mut pool = capture_workers(
        &agent,
        operation,
        &master,
        Arc::new(state),
        &directory,
        counters.clone(),
        Arc::new(AtomicBool::new(false)),
        ceilings,
    );
    assert_eq!(pool.threads.len(), 16);
    assert_eq!(pool.receiver.max_capacity(), 32);
    let mut captured_files = HashSet::new();
    while let Some(receipt) = pool.receiver.blocking_recv() {
        let receipt = receipt.unwrap();
        assert!(receipt.done);
        for (index, _) in receipt.files {
            assert!(captured_files.insert(index));
        }
    }
    assert_eq!(captured_files.len(), 64);
    drop(pool); // Close queues and join every admitted thread.

    let output = agent
        .config
        .paths
        .packs
        .join(prepared.manifest.bundle_id.to_string());
    let cas = LocalCas::new(agent.config.paths.cache.join("cas/chunks")).unwrap();
    let lease = cas.lease().unwrap();
    let mut plans = Vec::new();
    for index in 0..64 {
        let payload = format!("automatic-ceiling-codec-job-{index}");
        let hash = noland_cas::blake3_hex(payload.as_bytes());
        let stored = lease
            .capture_streaming_prehashed(&hash, payload.as_bytes())
            .unwrap();
        plans.push(Plan {
            index,
            chunks: vec![CapturedChunk {
                file: index,
                hash,
                source: stored.source,
                lease: lease.root().to_path_buf(),
                small_save: true,
            }],
        });
    }
    lease.checkpoint().unwrap();
    let mut codecs = pack_workers(
        &agent,
        &master,
        operation,
        &directory,
        &output,
        ceilings.codecs,
        64,
        counters,
        Arc::new(AtomicBool::new(false)),
    );
    assert_eq!(codecs.threads.len(), 16);
    for plan in plans {
        // Every admitted codec has independent real work; no throughput/timing
        // assertion or host-dependent CPU detector is involved.
        assert!(codecs.sender.as_ref().unwrap().try_send(plan).is_ok());
    }
    codecs.sender.take();
    let mut sealed = HashSet::new();
    while let Some(result) = codecs.receiver.blocking_recv() {
        let (index, pack, _) = result.unwrap();
        assert_eq!(pack.entries.len(), 1);
        assert!(sealed.insert(index));
    }
    assert_eq!(sealed.len(), 64);
    drop(codecs);
    drop(lease);
    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}
