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
