use super::*;
use crate::tests::{crash_test_plan, test_dir, test_file, test_manifest, test_plan};
use noland_state_core::{AgentPaths, ManifestFile, PersistenceClass, SemanticRole};
use std::path::Path;
use std::sync::{atomic::AtomicUsize, mpsc};

fn file(name: &str, bytes: &[u8]) -> ManifestFile {
    test_file(
        name,
        PersistenceClass::PersistentState,
        SemanticRole::UserState,
        bytes,
    )
}

#[test]
fn assembly_overlaps_a_blocked_verifier_and_reuses_workers_across_publications() {
    let root = test_dir("pipeline-overlap");
    let plan = test_plan(
        &root,
        test_manifest(vec![
            file("game/first", b"first"),
            file("game/second", b"second"),
            file("game/third", b"third"),
        ]),
    );
    let roots = LogicalRootMap::from_home(root.join("home"));
    let mut transaction = RestoreTransaction::new(&plan, &roots, None);
    let mut pipeline = Pipeline::new(&mut transaction, 1, |_| Ok(())).unwrap();
    pipeline.overlap_file_syncs = true;
    let threads = Arc::new(Mutex::new(Vec::new()));
    let worker_threads = Arc::clone(&threads);
    let (started, entered) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let released = Mutex::new(released);
    pipeline.verification = VerificationPool::start(1, move |task, stats, stopped, buffer| {
        worker_threads
            .lock()
            .unwrap()
            .push(std::thread::current().id());
        if task.pending.planned.manifest_index == 0 {
            started.send(()).unwrap();
            released
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        }
        verification::verify_file(task, stats, stopped, buffer)
    })
    .unwrap();

    pipeline
        .consume(&noland_cas::blake3_hex(b"first"), b"first")
        .unwrap();
    pipeline.dispatch_assembled().unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    pipeline
        .consume(&noland_cas::blake3_hex(b"second"), b"second")
        .unwrap();
    let second = root.join("home/.local/share/game/second");
    let partial = destination_partial_path(&second, plan.restore_id).unwrap();
    assert_eq!(fs::read(partial).unwrap(), b"second");
    assert!(
        !second.exists(),
        "assembly must not publish unverified data"
    );
    assert_eq!(pipeline.progress.assembled_bytes, 11);
    assert_eq!(pipeline.progress.completed_entries, 0);
    release.send(()).unwrap();
    pipeline.flush_ready().unwrap();
    pipeline
        .consume(&noland_cas::blake3_hex(b"third"), b"third")
        .unwrap();
    let report = pipeline.finish().unwrap();
    transaction.commit().unwrap();
    let threads = threads.lock().unwrap();
    assert_eq!(threads.len(), 3);
    assert!(
        threads.iter().all(|id| *id == threads[0]),
        "workers must survive batch boundaries"
    );
    assert_eq!(report.io.file_syncs, 3);
    assert_eq!(report.restore.published_entries, 3);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn admitted_verification_pool_really_runs_more_than_sixteen_workers() {
    let host = WorkerResources {
        logical_cpus: 64,
        available_memory: Some(32 * PACK_MAX),
        io_pressure: false,
    };
    let requested = PipelineOptions {
        reconstruction_workers: 24,
        overlap_file_syncs: true,
        ..PipelineOptions::for_resources(host, TransferWorkload::Throughput)
    };
    let provider = noland_storage::LocalStorage::new("unused-wide-verification");
    let (admitted, io) = requested.resolve(host, &provider).unwrap();
    let workers = admitted.reconstruction_workers;
    assert!(workers > 16);
    let root = test_dir("pipeline-wide-verification");
    let payload = b"authenticated read-back";
    let plan = test_plan(
        &root,
        test_manifest(
            (0..workers)
                .map(|index| file(&format!("game/{index}"), payload))
                .collect(),
        ),
    );
    let roots = LogicalRootMap::from_home(root.join("home"));
    let mut transaction = RestoreTransaction::new(&plan, &roots, None);
    let mut readback = 0;
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let worker_release = Arc::clone(&release);
    let (started, entered) = mpsc::channel();
    // Use the same constructor as production, passing its final admitted count
    // into the factory. Only the verifier body is gated, never the pool width.
    let mut pipeline = Pipeline::with_verification_factory(
        &mut transaction,
        admitted.reconstruction_workers,
        io,
        move |resolved_workers| {
            assert_eq!(resolved_workers, workers);
            VerificationPool::start(resolved_workers, move |task, stats, stopped, buffer| {
                started.send(std::thread::current().id()).unwrap();
                let (ready, notify) = &*worker_release;
                let (released, _) = notify
                    .wait_timeout_while(ready.lock().unwrap(), Duration::from_secs(5), |ready| {
                        !*ready
                    })
                    .unwrap();
                if !*released {
                    return Err(StateError::Storage(
                        "verification pool was serialized".into(),
                    ));
                }
                verification::verify_file(task, stats, stopped, buffer)
            })
        },
        |sample| {
            assert_eq!(sample.io.effective_verification_workers, workers as u64);
            assert!(sample.io.overlap_file_syncs);
            if sample.phase != "checking_existing_files" {
                assert_eq!(sample.io.verification_pool_workers, workers as u64);
            }
            readback = sample.readback_bytes;
            Ok(())
        },
    )
    .unwrap();
    // EOF/explicit dispatch, not elapsed wall time, releases this complete
    // window. Slow test filesystems must not dispatch a smaller gated batch.
    pipeline.publication_delay = Duration::MAX;
    for pending in pipeline.pending.values_mut() {
        pending.readback = true;
    }
    pipeline
        .consume(&noland_cas::blake3_hex(payload), payload)
        .unwrap();
    pipeline.dispatch_assembled().unwrap();
    let mut threads = Vec::new();
    for _ in 0..workers {
        let thread = entered.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            !threads.contains(&thread),
            "one worker consumed multiple gated jobs"
        );
        threads.push(thread);
    }
    *release.0.lock().unwrap() = true;
    release.1.notify_all();
    let report = pipeline.finish().unwrap();
    transaction.commit().unwrap();
    assert_eq!(report.io.peak_verification_workers, workers as u64);
    assert_eq!(report.io.verification_pool_workers, workers as u64);
    assert_eq!(report.io.file_syncs, workers as u64);
    assert_eq!(report.restore.published_entries, workers);
    assert_eq!(readback, workers as u64 * payload.len() as u64);
    for index in 0..workers {
        assert_eq!(
            fs::read(root.join(format!("home/.local/share/game/{index}"))).unwrap(),
            payload
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn cancellation_joins_an_active_verifier_before_rollback_removes_partials() {
    let root = test_dir("pipeline-verifier-cancel");
    let plan = test_plan(&root, test_manifest(vec![file("game/save", b"new-save")]));
    let roots = LogicalRootMap::from_home(root.join("home"));
    let destination = root.join("home/.local/share/game/save");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::write(&destination, b"original-save").unwrap();
    let mut transaction = RestoreTransaction::new(&plan, &roots, None);
    let mut pipeline = Pipeline::new(&mut transaction, 1, |_| Ok(())).unwrap();
    let (started, entered) = mpsc::channel();
    let finished = Arc::new(AtomicBool::new(false));
    let worker_finished = Arc::clone(&finished);
    pipeline.verification = VerificationPool::start(1, move |task, _, stopped, _| {
        started.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !stopped.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::park_timeout(Duration::from_millis(1));
        }
        assert!(stopped.load(Ordering::Acquire));
        assert!(
            task.pending.partial.is_file(),
            "rollback ran ahead of an active verifier"
        );
        // Model the final write/metadata syscall of a worker already in flight.
        fs::write(&task.pending.partial, b"last-worker-write").unwrap();
        worker_finished.store(true, Ordering::Release);
        Err(StateError::Storage("cancelled verifier".into()))
    })
    .unwrap();
    pipeline
        .consume(&noland_cas::blake3_hex(b"new-save"), b"new-save")
        .unwrap();
    pipeline.dispatch_assembled().unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    drop(pipeline);
    assert!(
        finished.load(Ordering::Acquire),
        "drop must join, not detach workers"
    );
    drop(transaction);
    assert_eq!(fs::read(&destination).unwrap(), b"original-save");
    assert!(!destination_partial_path(&destination, plan.restore_id)
        .unwrap()
        .exists());
    assert!(!plan.staging.exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn verifier_panics_fail_closed_instead_of_losing_an_inflight_task() {
    let root = test_dir("pipeline-verifier-panic");
    let plan = test_plan(&root, test_manifest(vec![file("game/save", b"save")]));
    let roots = LogicalRootMap::from_home(root.join("home"));
    let mut transaction = RestoreTransaction::new(&plan, &roots, None);
    let mut pipeline = Pipeline::new(&mut transaction, 2, |_| Ok(())).unwrap();
    pipeline.verification =
        VerificationPool::start(2, |_, _, _, _| panic!("injected verifier panic")).unwrap();
    let result = pipeline.consume(&noland_cas::blake3_hex(b"save"), b"save");
    let error = result.and_then(|_| pipeline.finish()).unwrap_err();
    assert!(error.to_string().contains("worker panicked"));
    drop(transaction);
    assert!(!root.join("home/.local/share/game/save").exists());
    assert!(!plan.staging.exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn verified_partial_replacement_is_rejected_before_publication() {
    let root = test_dir("pipeline-verified-replaced");
    let plan = test_plan(&root, test_manifest(vec![file("game/save", b"save")]));
    let roots = LogicalRootMap::from_home(root.join("home"));
    let unrelated = root.join("unrelated");
    fs::write(&unrelated, b"keep-me").unwrap();
    {
        let mut transaction = RestoreTransaction::new(&plan, &roots, None);
        let mut pipeline = Pipeline::new(&mut transaction, 1, |_| Ok(())).unwrap();
        pipeline
            .consume(&noland_cas::blake3_hex(b"save"), b"save")
            .unwrap();
        pipeline.dispatch_assembled().unwrap();
        pipeline.collect_verified(Duration::from_secs(5)).unwrap();
        let partial = &pipeline.ready[0].pending.partial;
        fs::remove_file(partial).unwrap();
        std::os::unix::fs::symlink(&unrelated, partial).unwrap();
        assert!(pipeline.finish().is_err());
    }
    assert_eq!(fs::read(&unrelated).unwrap(), b"keep-me");
    assert!(!root.join("home/.local/share/game/save").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn recovery_removes_verified_but_unpublished_partials_and_keeps_original_saves() {
    let root = test_dir("pipeline-verified-crash");
    let paths = AgentPaths::from_roots(root.join("state"), root.join("run"));
    paths.ensure_dirs().unwrap();
    let plan = crash_test_plan(
        &paths,
        test_manifest(vec![file("game/save", b"save"), file("game/empty", b"")]),
    );
    let roots = LogicalRootMap::from_home(root.join("home"));
    let destination = root.join("home/.local/share/game/save");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::write(&destination, b"original-save").unwrap();
    let mut transaction = RestoreTransaction::new(&plan, &roots, None);
    let mut pipeline = Pipeline::new(&mut transaction, 2, |_| Ok(())).unwrap();
    pipeline
        .consume(&noland_cas::blake3_hex(b"save"), b"save")
        .unwrap();
    pipeline.dispatch_assembled().unwrap();
    pipeline.collect_verified(Duration::from_secs(5)).unwrap();
    assert_eq!(pipeline.ready.len(), 1);
    assert_eq!(fs::read(&destination).unwrap(), b"original-save");
    let partial = pipeline.ready[0].pending.partial.clone();
    assert!(partial.exists());
    drop(pipeline);
    std::mem::forget(transaction);
    let report = crate::recover_interrupted_restores(&paths).unwrap();
    assert_eq!(report.workspaces_rolled_back, 1);
    assert!(report.failures.is_empty());
    assert_eq!(fs::read(&destination).unwrap(), b"original-save");
    assert!(!partial.exists());
    assert!(!plan.staging.exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn byte_window_admits_an_oversized_file_alone_and_flushes_small_trailing_files() {
    let root = test_dir("pipeline-byte-window");
    let payload = vec![0x37; MAX_UNPUBLISHED_BYTES as usize + 1];
    let hash = noland_cas::blake3_hex(&payload);
    let plan = test_plan(
        &root,
        test_manifest(vec![
            file("game/large-a", &payload),
            file("game/large-b", &payload),
            file("game/save", b"save"),
        ]),
    );
    let roots = LogicalRootMap::from_home(root.join("home"));
    let mut transaction = RestoreTransaction::new(&plan, &roots, None);
    let mut pipeline = Pipeline::new(&mut transaction, 2, |_| Ok(())).unwrap();
    pipeline.consume(&hash, &payload).unwrap();
    pipeline
        .consume(&noland_cas::blake3_hex(b"save"), b"save")
        .unwrap();
    let report = pipeline.finish().unwrap();
    assert_eq!(report.io.peak_unpublished_bytes, payload.len() as u64);
    assert_eq!(report.io.peak_unpublished_files, 1);
    assert_eq!(report.io.file_syncs, 3);
    assert_eq!(report.restore.published_entries, 3);
    transaction.commit().unwrap();
    for name in ["large-a", "large-b"] {
        assert_eq!(
            noland_cas::blake3_file(&root.join("home/.local/share/game").join(name)).unwrap(),
            hash
        );
    }
    assert_eq!(
        fs::read(root.join("home/.local/share/game/save")).unwrap(),
        b"save"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn time_trigger_publishes_small_verified_batches_without_another_chunk() {
    let root = test_dir("pipeline-publication-time");
    let plan = test_plan(&root, test_manifest(vec![file("game/save", b"save")]));
    let roots = LogicalRootMap::from_home(root.join("home"));
    let mut transaction = RestoreTransaction::new(&plan, &roots, None);
    let mut pipeline = Pipeline::new(&mut transaction, 1, |_| Ok(())).unwrap();
    pipeline
        .consume(&noland_cas::blake3_hex(b"save"), b"save")
        .unwrap();
    pipeline.assembled_since = Some(Instant::now() - PUBLICATION_DELAY);
    pipeline.tick().unwrap();
    pipeline.collect_verified(Duration::from_secs(5)).unwrap();
    pipeline.tick().unwrap();
    assert_eq!(pipeline.progress.completed_entries, 1);
    assert_eq!(pipeline.progress.io.directory_syncs, 1);
    let report = pipeline.finish().unwrap();
    assert_eq!(report.io.publication_batches, 1);
    transaction.commit().unwrap();
    assert_eq!(
        fs::read(root.join("home/.local/share/game/save")).unwrap(),
        b"save"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn empty_files_use_the_same_bounded_verification_and_durability_path() {
    let root = test_dir("pipeline-empty-files");
    let count = MAX_UNPUBLISHED_FILES * 2 + 1;
    let files = (0..count)
        .map(|index| {
            let mut file = file(&format!("game/empty-{index}"), b"");
            file.chunks.clear();
            file
        })
        .collect();
    let plan = test_plan(&root, test_manifest(files));
    let roots = LogicalRootMap::from_home(root.join("home"));
    let mut transaction = RestoreTransaction::new(&plan, &roots, None);
    let calls = Arc::new(AtomicUsize::new(0));
    let callback_calls = Arc::clone(&calls);
    let pipeline = Pipeline::new(&mut transaction, 2, move |_| {
        callback_calls.fetch_add(1, Ordering::Relaxed);
        Ok(())
    })
    .unwrap();
    let report = pipeline.finish().unwrap();
    assert_eq!(report.io.file_syncs, count as u64);
    assert!(report.io.peak_unpublished_files <= MAX_UNPUBLISHED_FILES as u64);
    assert_eq!(report.io.peak_unpublished_bytes, 0);
    transaction.commit().unwrap();
    assert!(calls.load(Ordering::Relaxed) > 0);
    for index in 0..count {
        assert_eq!(
            fs::metadata(root.join(format!("home/.local/share/game/empty-{index}")))
                .unwrap()
                .len(),
            0
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_late_hash_failure_rolls_back_an_already_published_window() {
    let root = test_dir("pipeline-late-hash-failure");
    let mut files = (0..PUBLICATION_BATCH)
        .map(|index| file(&format!("game/asset-{index}"), b"asset"))
        .collect::<Vec<_>>();
    files.push(file("game/save", b"new-save"));
    let plan = test_plan(&root, test_manifest(files));
    let roots = LogicalRootMap::from_home(root.join("home"));
    let game = root.join("home/.local/share/game");
    fs::create_dir_all(&game).unwrap();
    fs::write(game.join("save"), b"original-save").unwrap();
    fs::write(game.join("unrelated"), b"keep-me").unwrap();
    {
        let mut transaction = RestoreTransaction::new(&plan, &roots, None);
        let mut pipeline = Pipeline::new(&mut transaction, 4, |_| Ok(())).unwrap();
        pipeline.overlap_file_syncs = true;
        pipeline
            .consume(&noland_cas::blake3_hex(b"asset"), b"asset")
            .unwrap();
        pipeline.flush_ready().unwrap();
        assert_eq!(
            pipeline.progress.completed_entries,
            PUBLICATION_BATCH as u64
        );
        assert!(game.join("asset-0").is_file());
        let result = pipeline.consume(&noland_cas::blake3_hex(b"new-save"), b"bad-save");
        assert!(result.and_then(|_| pipeline.finish()).is_err());
    }
    assert_eq!(fs::read(game.join("save")).unwrap(), b"original-save");
    assert_eq!(fs::read(game.join("unrelated")).unwrap(), b"keep-me");
    for index in 0..PUBLICATION_BATCH {
        assert!(!game.join(format!("asset-{index}")).exists());
    }
    assert!(!plan.staging.exists());
    fs::remove_dir_all(root).unwrap();
}

// Hold a real authenticated pack's EOF until the assembler publishes a file.
// This catches an idle receive loop that never services its publication timer.
struct PausedStream {
    local: noland_storage::LocalStorage,
    bytes: Vec<u8>,
    resume: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    replay: bool,
}

#[async_trait::async_trait]
impl SharedStorageProvider for PausedStream {
    async fn health_check(&self) -> Result<noland_storage::Health> {
        self.local.health_check().await
    }
    async fn ensure_root(&self) -> Result<()> {
        self.local.ensure_root().await
    }
    async fn stat(&self, key: &RemoteKey) -> Result<Option<noland_storage::RemoteMeta>> {
        self.local.stat(key).await
    }
    async fn upload_immutable(
        &self,
        path: &Path,
        key: &RemoteKey,
    ) -> Result<noland_storage::RemoteMeta> {
        self.local.upload_immutable(path, key).await
    }
    async fn download(&self, key: &RemoteKey, path: &Path) -> Result<()> {
        self.local.download(key, path).await
    }
    async fn list_prefix(&self, key: &RemoteKey) -> Result<Vec<noland_storage::RemoteEntry>> {
        self.local.list_prefix(key).await
    }
    async fn put_small_versioned(
        &self,
        bytes: bytes::Bytes,
        key: &RemoteKey,
    ) -> Result<noland_storage::RemoteMeta> {
        self.local.put_small_versioned(bytes, key).await
    }
    fn supports_streaming_downloads(&self) -> bool {
        true
    }

    async fn download_stream_observed(
        &self,
        _: &RemoteKey,
        _: &Path,
        consumer: &dyn DownloadStreamConsumer,
        _: Option<&(dyn DownloadObserver + '_)>,
    ) -> Result<()> {
        consumer.restart().await?;
        consumer.write(&self.bytes).await?;
        if self.replay {
            // A stalled process can be restarted after a full authenticated
            // record was already accepted, but before process EOF.
            consumer.restart().await?;
            consumer.write(&self.bytes).await?;
        }
        if let Some(resume) = self.resume.lock().await.take() {
            tokio::time::timeout(Duration::from_secs(5), resume)
                .await
                .map_err(|_| StateError::Storage("publication stalled on idle download".into()))?
                .map_err(|_| StateError::Storage("publication callback stopped".into()))?;
        }
        consumer.finish().await
    }
}

#[tokio::test]
async fn streaming_publishes_during_idle_download_and_reports_final_hash_failures() {
    let root = test_dir("pipeline-idle-stream");
    let master = MasterKey::generate();
    let payload = b"restored save";
    let packs = noland_pack::pack_chunks(
        &root.join("built"),
        &master,
        vec![(noland_cas::blake3_hex(payload), payload.to_vec())],
        |_| false,
    )
    .unwrap();
    let (published, resume) = tokio::sync::oneshot::channel();
    let mut published = Some(published);
    let storage = PausedStream {
        local: noland_storage::LocalStorage::new(root.join("unused-remote")),
        bytes: fs::read(&packs[0].path).unwrap(),
        resume: tokio::sync::Mutex::new(Some(resume)),
        replay: false,
    };
    let roots = LogicalRootMap::from_home(root.join("home"));
    let destination = root.join("home/.local/share/game/save");
    let plan = test_plan(
        &root.join("first"),
        test_manifest(vec![file("game/save", payload)]),
    );
    let report = restore_pipelined(
        &storage,
        &master,
        &plan,
        &packs[0].entries,
        &roots,
        None,
        None,
        PipelineOptions {
            overlap_file_syncs: true,
            ..PipelineOptions::default()
        },
        |sample| {
            if sample.completed_entries == 1 {
                assert_eq!(fs::read(&destination).unwrap(), payload);
                assert_eq!(sample.verified_bytes, payload.len() as u64);
                if let Some(published) = published.take() {
                    published.send(()).unwrap();
                }
            }
            Ok(())
        },
    )
    .await
    .unwrap();
    assert_eq!(report.restore.published_entries, 1);
    assert!(published.is_none());

    let mut bad_file = file("game/save", payload);
    bad_file.file_hash = noland_cas::blake3_hex(b"wrong full-file digest");
    let bad_plan = test_plan(&root.join("bad-hash"), test_manifest(vec![bad_file]));
    let error = restore_pipelined(
        &storage,
        &master,
        &bad_plan,
        &packs[0].entries,
        &roots,
        None,
        None,
        PipelineOptions::default(),
        |_| Ok(()),
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("reconstructed file hash mismatch"),
        "{error}"
    );
    assert_eq!(fs::read(&destination).unwrap(), payload);
    assert!(!bad_plan.staging.exists());
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn restarted_authenticated_stream_never_rewrites_or_double_counts_accepted_files() {
    let root = test_dir("pipeline-restarted-stream");
    let master = MasterKey::generate();
    let payload = b"restored save";
    let packs = noland_pack::pack_chunks(
        &root.join("built"),
        &master,
        vec![(noland_cas::blake3_hex(payload), payload.to_vec())],
        |_| false,
    )
    .unwrap();
    let storage = PausedStream {
        local: noland_storage::LocalStorage::new(root.join("unused-remote")),
        bytes: fs::read(&packs[0].path).unwrap(),
        resume: tokio::sync::Mutex::new(None),
        replay: true,
    };
    let roots = LogicalRootMap::from_home(root.join("home"));
    let plan = test_plan(
        &root.join("restore"),
        test_manifest(vec![file("game/save", payload)]),
    );
    let mut verified = 0;
    let report = restore_pipelined(
        &storage,
        &master,
        &plan,
        &packs[0].entries,
        &roots,
        None,
        None,
        PipelineOptions::default(),
        |sample| {
            verified = sample.verified_bytes;
            assert!(sample.completed_entries <= 1);
            Ok(())
        },
    )
    .await
    .unwrap();
    assert_eq!(report.restore.published_entries, 1);
    assert_eq!(report.io.file_syncs, 1);
    assert_eq!(report.io.assembly_opens, 1);
    assert_eq!(verified, payload.len() as u64);
    assert_eq!(
        fs::read(root.join("home/.local/share/game/save")).unwrap(),
        payload
    );
    fs::remove_dir_all(root).unwrap();
}

/// The first pack retains its network slot until another slot has completed and
/// refilled AND the assembler has verified/published real authenticated data.
struct RollingStreams {
    local: noland_storage::LocalStorage,
    packs: BTreeMap<String, Vec<u8>>,
    initial_group: tokio::sync::Barrier,
    initial_width: usize,
    started: AtomicUsize,
    refilled: AtomicBool,
    published: AtomicBool,
    slow_finished: AtomicBool,
    changed: tokio::sync::Notify,
}

#[async_trait::async_trait]
impl SharedStorageProvider for RollingStreams {
    async fn health_check(&self) -> Result<noland_storage::Health> {
        self.local.health_check().await
    }
    async fn ensure_root(&self) -> Result<()> {
        self.local.ensure_root().await
    }
    async fn stat(&self, key: &RemoteKey) -> Result<Option<noland_storage::RemoteMeta>> {
        self.local.stat(key).await
    }
    async fn upload_immutable(
        &self,
        path: &Path,
        key: &RemoteKey,
    ) -> Result<noland_storage::RemoteMeta> {
        self.local.upload_immutable(path, key).await
    }
    async fn download(&self, key: &RemoteKey, path: &Path) -> Result<()> {
        self.local.download(key, path).await
    }
    async fn list_prefix(&self, key: &RemoteKey) -> Result<Vec<noland_storage::RemoteEntry>> {
        self.local.list_prefix(key).await
    }
    async fn put_small_versioned(
        &self,
        bytes: bytes::Bytes,
        key: &RemoteKey,
    ) -> Result<noland_storage::RemoteMeta> {
        self.local.put_small_versioned(bytes, key).await
    }
    fn supports_streaming_downloads(&self) -> bool {
        true
    }
    fn transfer_tuning(&self) -> TransferTuning {
        TransferTuning {
            max_parallel_downloads: self.initial_width,
            ..TransferTuning::default()
        }
    }
    async fn download_stream_observed(
        &self,
        key: &RemoteKey,
        _: &Path,
        consumer: &dyn DownloadStreamConsumer,
        _: Option<&(dyn DownloadObserver + '_)>,
    ) -> Result<()> {
        let order = self.started.fetch_add(1, Ordering::AcqRel);
        if order == self.initial_width {
            self.refilled.store(true, Ordering::Release);
            self.changed.notify_waiters();
        }
        consumer.restart().await?;
        if order < self.initial_width {
            tokio::time::timeout(Duration::from_secs(5), self.initial_group.wait())
                .await
                .map_err(|_| StateError::Storage("pack decoders were serialized".into()))?;
        }
        consumer.write(&self.packs[key.as_str()]).await?;
        if order == 0 {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let notified = self.changed.notified();
                    tokio::pin!(notified);
                    notified.as_mut().enable();
                    if self.refilled.load(Ordering::Acquire)
                        && self.published.load(Ordering::Acquire)
                    {
                        break;
                    }
                    notified.await;
                }
            })
            .await
            .map_err(|_| StateError::Storage("restore waited for the slowest pack".into()))?;
        }
        consumer.finish().await?;
        if order == 0 {
            self.slow_finished.store(true, Ordering::Release);
        }
        Ok(())
    }
}

fn rolling_fixture(
    root: &Path,
    master: &MasterKey,
    payloads: &[&[u8]],
    initial_width: usize,
    corrupt_last: bool,
) -> (RollingStreams, Vec<PackIndexEntry>) {
    let mut index = Vec::new();
    let mut streams = BTreeMap::new();
    for (number, payload) in payloads.iter().enumerate() {
        // Separate pack builders produce real, distinct encrypted packs without
        // allocating gigabytes merely to cross a pack-format boundary.
        let pack = noland_pack::pack_chunks(
            &root.join(format!("built-{number}")),
            master,
            vec![(noland_cas::blake3_hex(payload), payload.to_vec())],
            |_| false,
        )
        .unwrap()
        .remove(0);
        let mut bytes = fs::read(&pack.path).unwrap();
        if corrupt_last && number == payloads.len() - 1 {
            *bytes.last_mut().unwrap() ^= 1;
        }
        streams.insert(pack_key(&pack.pack_id), bytes);
        index.extend(pack.entries);
    }
    (
        RollingStreams {
            local: noland_storage::LocalStorage::new(root.join("unused-remote")),
            packs: streams,
            initial_group: tokio::sync::Barrier::new(initial_width),
            initial_width,
            started: AtomicUsize::new(0),
            refilled: AtomicBool::new(false),
            published: AtomicBool::new(false),
            slow_finished: AtomicBool::new(false),
            changed: tokio::sync::Notify::new(),
        },
        index,
    )
}

#[tokio::test]
async fn authenticated_multi_pack_restore_verifies_during_download_and_refills_completed_slots() {
    let root = test_dir("pipeline-parallel-streams");
    let master = MasterKey::generate();
    let payloads = [
        b"first pack".as_slice(),
        b"second pack",
        b"third pack",
        b"fourth pack",
        b"fifth pack",
        b"sixth pack",
    ];
    let (storage, index) = rolling_fixture(&root, &master, &payloads, 5, false);
    let plan = test_plan(
        &root.join("restore"),
        test_manifest(
            payloads
                .iter()
                .enumerate()
                .map(|(index, payload)| file(&format!("game/{index}"), payload))
                .collect(),
        ),
    );
    let roots = LogicalRootMap::from_home(root.join("home"));
    let report = restore_pipelined_for_resources(
        &storage,
        &master,
        &plan,
        &index,
        &roots,
        None,
        None,
        PipelineOptions {
            workload: TransferWorkload::Throughput,
            max_parallel_packs: 5,
            reconstruction_workers: 2,
            buffered_chunks: 8,
            max_inflight_pack_bytes: PACK_MAX,
            overlap_file_syncs: true,
        },
        WorkerResources {
            logical_cpus: 16,
            available_memory: Some(8 * PACK_MAX),
            io_pressure: false,
        },
        |sample| {
            assert_eq!(sample.io.requested_parallel_packs, 5);
            assert_eq!(sample.io.effective_parallel_packs, 5);
            assert_eq!(sample.io.requested_verification_workers, 2);
            assert_eq!(sample.io.effective_verification_workers, 2);
            assert_eq!(sample.io.requested_buffered_chunks, 8);
            assert_eq!(sample.io.effective_buffered_chunks, 5);
            if sample.completed_entries >= 2 && !storage.published.load(Ordering::Acquire) {
                assert!(!storage.slow_finished.load(Ordering::Acquire));
                assert!(sample.verified_bytes > 0);
                storage.published.store(true, Ordering::Release);
                storage.changed.notify_waiters();
            }
            Ok(())
        },
    )
    .await
    .unwrap();
    assert!(storage.refilled.load(Ordering::Acquire));
    assert!(storage.published.load(Ordering::Acquire));
    assert!(storage.slow_finished.load(Ordering::Acquire));
    assert_eq!(report.packs_downloaded, payloads.len());
    assert_eq!(report.restore.published_entries, payloads.len());
    assert_eq!(report.io.peak_decode_workers, 5);
    assert!(report.io.peak_verification_workers > 0);
    assert_eq!(report.io.file_syncs, payloads.len() as u64);
    for (index, payload) in payloads.iter().enumerate() {
        assert_eq!(
            fs::read(root.join(format!("home/.local/share/game/{index}"))).unwrap(),
            *payload
        );
    }
    assert!(!plan.staging.exists());
    assert!(!plan.pack_cache.exists());
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn parallel_stream_corruption_and_callback_cancellation_restore_every_original_exactly() {
    for corrupt in [false, true] {
        let root = test_dir("pipeline-parallel-rollback");
        let master = MasterKey::generate();
        let payloads = [b"first pack".as_slice(), b"second pack", b"third pack"];
        let (storage, index) = rolling_fixture(&root, &master, &payloads, 2, corrupt);
        let plan = test_plan(
            &root.join("restore"),
            test_manifest(
                payloads
                    .iter()
                    .enumerate()
                    .map(|(index, payload)| file(&format!("game/{index}"), payload))
                    .collect(),
            ),
        );
        let home = root.join("home");
        let game = home.join(".local/share/game");
        fs::create_dir_all(&game).unwrap();
        for index in 0..payloads.len() {
            fs::write(game.join(index.to_string()), format!("original-{index}")).unwrap();
        }
        fs::write(game.join("unrelated"), b"keep-me").unwrap();
        let mut cancelled_at_publication = false;
        let error = restore_pipelined_for_resources(
            &storage,
            &master,
            &plan,
            &index,
            &LogicalRootMap::from_home(&home),
            None,
            None,
            PipelineOptions {
                workload: TransferWorkload::Throughput,
                max_parallel_packs: 2,
                reconstruction_workers: 2,
                buffered_chunks: 2,
                max_inflight_pack_bytes: PACK_MAX,
                overlap_file_syncs: true,
            },
            WorkerResources {
                logical_cpus: 8,
                available_memory: Some(8 * PACK_MAX),
                io_pressure: false,
            },
            |sample| {
                if sample.completed_entries >= 2 && storage.refilled.load(Ordering::Acquire) {
                    if !corrupt {
                        cancelled_at_publication = true;
                        return Err(StateError::Storage(
                            "cancel parallel restore after publication".into(),
                        ));
                    }
                    storage.published.store(true, Ordering::Release);
                    storage.changed.notify_waiters();
                }
                Ok(())
            },
        )
        .await
        .unwrap_err();
        assert!(storage.refilled.load(Ordering::Acquire));
        assert!(corrupt || cancelled_at_publication);
        assert!(
            !error.to_string().contains("serialized")
                && !error.to_string().contains("slowest pack"),
            "the fixture must exercise corruption/cancellation, not a timeout: {error}"
        );
        for index in 0..payloads.len() {
            let destination = game.join(index.to_string());
            assert_eq!(
                fs::read(&destination).unwrap(),
                format!("original-{index}").as_bytes()
            );
            assert!(!destination_partial_path(&destination, plan.restore_id)
                .unwrap()
                .exists());
        }
        assert_eq!(fs::read(game.join("unrelated")).unwrap(), b"keep-me");
        assert!(
            !plan.staging.exists(),
            "rollback must finish before returning"
        );
        fs::remove_dir_all(root).unwrap();
    }
}
