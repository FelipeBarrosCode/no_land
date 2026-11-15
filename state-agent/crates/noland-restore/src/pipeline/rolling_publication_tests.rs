use super::*;
use crate::tests::{crash_test_plan, test_dir, test_file, test_manifest, test_plan};
use noland_state_core::{AgentPaths, ManifestFile, PersistenceClass, SemanticRole};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::sync::{mpsc, Condvar};

fn file(name: &str, bytes: &[u8]) -> ManifestFile {
    let mut file = test_file(
        name,
        PersistenceClass::PersistentState,
        SemanticRole::UserState,
        bytes,
    );
    file.mode = Some(0o640);
    file
}

#[derive(Default)]
struct Gate {
    open: Mutex<bool>,
    changed: Condvar,
}

impl Gate {
    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.changed.notify_all();
    }

    fn wait(&self, stopped: &AtomicBool) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut open = self.open.lock().unwrap();
        while !*open && !stopped.load(Ordering::Acquire) {
            if Instant::now() >= deadline {
                return Err(StateError::Storage("verification gate timed out".into()));
            }
            // Pool cancellation must also release a fixture whose assertion
            // failed before the test could explicitly release its slow peer.
            open = self
                .changed
                .wait_timeout(open, Duration::from_millis(10))
                .unwrap()
                .0;
        }
        if stopped.load(Ordering::Acquire) {
            return Err(StateError::Storage("verification gate cancelled".into()));
        }
        Ok(())
    }
}

#[test]
fn ready_subset_publishes_and_refills_workers_before_a_slow_peer_finishes() {
    let root = test_dir("pipeline-ready-refill");
    let payloads = [b"slow".as_slice(), b"ready", b"refill-a", b"refill-b"];
    let plan = test_plan(
        &root,
        test_manifest(
            payloads
                .iter()
                .enumerate()
                .map(|(index, bytes)| file(&format!("game/{index}"), bytes))
                .collect(),
        ),
    );
    let roots = LogicalRootMap::from_home(root.join("home"));
    let game = root.join("home/.local/share/game");
    let mut transaction = RestoreTransaction::new(&plan, &roots, None);
    let mut final_bytes = (0, 0, 0);
    let mut pipeline = Pipeline::new(&mut transaction, 2, |sample| {
        final_bytes = (
            sample.assembled_bytes,
            sample.verified_bytes,
            sample.published_bytes,
        );
        Ok(())
    })
    .unwrap();
    pipeline.overlap_file_syncs = true;
    pipeline.publication_delay = Duration::MAX;
    let checkpoint = fs::read(plan.staging.join(crate::TRANSACTION_JOURNAL_FILE)).unwrap();
    let intents = fs::read(plan.staging.join("restore-intents.v1.log")).unwrap();
    let slow = Arc::new(Gate::default());
    let worker_slow = Arc::clone(&slow);
    let (started, entered) = mpsc::channel();
    pipeline.verification = VerificationPool::start(2, move |task, stats, stopped, buffer| {
        let index = task.pending.planned.manifest_index;
        if index == 0 {
            started.send(()).unwrap();
            worker_slow.wait(stopped)?;
        }
        verification::verify_file(task, stats, stopped, buffer)
    })
    .unwrap();
    for payload in &payloads[..2] {
        pipeline
            .consume(&noland_cas::blake3_hex(payload), payload)
            .unwrap();
    }
    pipeline.dispatch_assembled().unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    pipeline.collect_verified(Duration::from_secs(5)).unwrap();
    assert_eq!(pipeline.ready.len(), 1);
    assert_eq!(pipeline.ready[0].pending.planned.manifest_index, 1);
    for payload in &payloads[2..] {
        pipeline
            .consume(&noland_cas::blake3_hex(payload), payload)
            .unwrap();
    }
    assert_eq!(pipeline.assembled.len(), 2);
    assert_eq!(pipeline.progress.completed_entries, 0);

    // Expire the ready timer without sleeping, receiving another chunk, or
    // releasing file 0. The old cohort barrier could not publish file 1 here.
    pipeline.publication_delay = PUBLICATION_DELAY;
    pipeline.ready_since = Some(Instant::now() - PUBLICATION_DELAY);
    pipeline.tick().unwrap();
    assert_eq!(pipeline.progress.completed_entries, 1);
    assert_eq!(fs::read(game.join("1")).unwrap(), payloads[1]);
    assert!(!game.join("0").exists());
    assert_eq!(pipeline.progress.io.partial_publication_batches, 1);
    assert_eq!(pipeline.progress.io.verification_refill_batches, 1);
    assert!(pipeline.assembled.is_empty());
    assert_eq!(pipeline.verifying_files, 3);
    // Both next-window files must finish on the spare worker BEFORE the first
    // window's slow verifier is released. Observe the production results, not a
    // fixture notification sent before the pool has delivered its result.
    while pipeline.ready.len() < 2 {
        pipeline.collect_verified(Duration::from_secs(5)).unwrap();
    }
    assert_eq!(
        pipeline
            .ready
            .iter()
            .map(|verified| verified.pending.planned.manifest_index)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([2, 3])
    );
    while pipeline.progress.completed_entries < 3 {
        pipeline.wait_for_publication(false).unwrap();
    }
    assert_eq!(pipeline.verifying_files, 1);
    assert!(!game.join("0").exists());
    assert_eq!(
        fs::read(plan.staging.join(crate::TRANSACTION_JOURNAL_FILE)).unwrap(),
        checkpoint
    );
    assert_eq!(
        fs::read(plan.staging.join("restore-intents.v1.log")).unwrap(),
        intents
    );
    slow.release();
    let report = pipeline.finish().unwrap();
    transaction.commit().unwrap();
    assert_eq!(report.restore.published_entries, payloads.len());
    assert_eq!(report.io.file_syncs, payloads.len() as u64);
    let total_bytes = payloads.iter().map(|bytes| bytes.len() as u64).sum::<u64>();
    assert_eq!(final_bytes, (total_bytes, total_bytes, total_bytes));
    assert!(report.io.peak_unpublished_files <= MAX_UNPUBLISHED_FILES as u64);
    assert!(report.io.peak_unpublished_bytes <= MAX_UNPUBLISHED_BYTES);
    for (index, payload) in payloads.iter().enumerate() {
        let path = game.join(index.to_string());
        assert_eq!(fs::read(&path).unwrap(), *payload);
        assert_eq!(fs::metadata(path).unwrap().mode() & 0o7777, 0o640);
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn staged_hosts_keep_whole_cohorts_even_when_a_ready_timer_has_expired() {
    let root = test_dir("pipeline-staged-cohort");
    let plan = test_plan(
        &root,
        test_manifest(vec![
            file("game/slow", b"slow"),
            file("game/ready", b"ready"),
        ]),
    );
    let roots = LogicalRootMap::from_home(root.join("home"));
    let mut transaction = RestoreTransaction::new(&plan, &roots, None);
    let mut pipeline = Pipeline::new(&mut transaction, 2, |_| Ok(())).unwrap();
    pipeline.overlap_file_syncs = false;
    pipeline.publication_delay = Duration::MAX;
    let slow = Arc::new(Gate::default());
    let worker_slow = Arc::clone(&slow);
    pipeline.verification = VerificationPool::start(2, move |task, stats, stopped, buffer| {
        if task.pending.planned.manifest_index == 0 {
            worker_slow.wait(stopped)?;
        }
        verification::verify_file(task, stats, stopped, buffer)
    })
    .unwrap();
    for payload in [b"slow".as_slice(), b"ready"] {
        pipeline
            .consume(&noland_cas::blake3_hex(payload), payload)
            .unwrap();
    }
    pipeline.dispatch_assembled().unwrap();
    pipeline.collect_verified(Duration::from_secs(5)).unwrap();
    assert_eq!(pipeline.ready.len(), 1);
    pipeline.publication_delay = PUBLICATION_DELAY;
    pipeline.ready_since = Some(Instant::now() - PUBLICATION_DELAY);
    pipeline.tick().unwrap();
    assert_eq!(pipeline.progress.completed_entries, 0);
    assert!(!root.join("home/.local/share/game/ready").exists());
    slow.release();
    let report = pipeline.finish().unwrap();
    transaction.commit().unwrap();
    assert_eq!(report.restore.published_entries, 2);
    assert_eq!(report.io.file_syncs, 2);
    assert_eq!(report.io.directory_syncs, 1);
    assert_eq!(report.io.partial_publication_batches, 0);
    assert_eq!(report.io.verification_refill_batches, 0);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_full_unpublished_window_releases_ready_capacity_without_waiting_for_a_slow_peer() {
    gated_capacity_fixture(false);
}

#[test]
fn one_ready_file_does_not_publish_a_full_window_while_its_cohort_is_queued() {
    gated_capacity_fixture(true);
}

fn gated_capacity_fixture(pause_after_first_ready: bool) {
    let root = test_dir("pipeline-ready-capacity");
    let count = MAX_UNPUBLISHED_FILES + 1;
    // Distinct chunks let the fixture stop input exactly at the capacity bound,
    // without modifying the pipeline's consumers or splitting a shared chunk.
    let payloads = (0..count)
        .map(|index| format!("small-file-{index}").into_bytes())
        .collect::<Vec<_>>();
    let hashes = payloads
        .iter()
        .map(|payload| noland_cas::blake3_hex(payload))
        .collect::<Vec<_>>();
    let plan = test_plan(
        &root,
        test_manifest(
            payloads
                .iter()
                .enumerate()
                .map(|(index, payload)| file(&format!("game/{index}"), payload))
                .collect(),
        ),
    );
    let roots = LogicalRootMap::from_home(root.join("home"));
    let game = root.join("home/.local/share/game");
    let mut transaction = RestoreTransaction::new(&plan, &roots, None);
    let fast = Arc::new(Gate::default());
    let arm_capacity_wait = Arc::new(AtomicBool::new(false));
    let saw_capacity_wait = Arc::new(AtomicBool::new(false));
    let progress_fast = Arc::clone(&fast);
    let progress_arm = Arc::clone(&arm_capacity_wait);
    let progress_saw_wait = Arc::clone(&saw_capacity_wait);
    let mut pipeline = Pipeline::new(&mut transaction, 2, move |sample| {
        if progress_arm.swap(false, Ordering::AcqRel) {
            // Only the capacity wait is armed. It must report a waiting turn
            // BEFORE publishing the isolated ready file; that observation,
            // rather than a sleep, releases the rest of the fast cohort.
            if sample.completed_entries != 0 {
                return Err(StateError::Storage(
                    "capacity wait published one file ahead of its queued cohort".into(),
                ));
            }
            progress_saw_wait.store(true, Ordering::Release);
            progress_fast.release();
        }
        Ok(())
    })
    .unwrap();
    pipeline.overlap_file_syncs = true;
    // The timer is disabled, but the count trigger still applies. Both workers
    // must remain gated until all MAX_UNPUBLISHED_FILES are admitted.
    pipeline.publication_delay = Duration::MAX;
    // Capture manifest identities in the explicit chunk delivery order, rather
    // than assuming preflight's lexicographic path order is numeric index order.
    let delivery_indices = hashes
        .iter()
        .map(|hash| {
            let placements = pipeline.consumers.get(hash).unwrap();
            assert_eq!(placements.len(), 1);
            placements[0].index
        })
        .collect::<Vec<_>>();
    let slow_index = delivery_indices[0];
    let first_ready_index = delivery_indices[1];
    let initial_cohort = delivery_indices
        .iter()
        .take(PUBLICATION_BATCH)
        .copied()
        .collect::<BTreeSet<_>>();
    assert_eq!(initial_cohort.len(), PUBLICATION_BATCH);
    let mut initial_fast = initial_cohort.clone();
    assert!(initial_fast.remove(&slow_index));
    let slow_destination = pipeline.pending[&slow_index].planned.destination.clone();
    let slow = Arc::new(Gate::default());
    let worker_slow = Arc::clone(&slow);
    let worker_fast = Arc::clone(&fast);
    let first_ready = Arc::new(Gate::default());
    let worker_first_ready = Arc::clone(&first_ready);
    pipeline.verification = VerificationPool::start(2, move |task, stats, stopped, buffer| {
        if task.pending.planned.manifest_index == slow_index {
            worker_slow.wait(stopped)?;
        } else if task.pending.planned.manifest_index == first_ready_index {
            worker_first_ready.wait(stopped)?;
        } else {
            worker_fast.wait(stopped)?;
        }
        verification::verify_file(task, stats, stopped, buffer)
    })
    .unwrap();
    for (hash, payload) in hashes.iter().zip(&payloads).take(MAX_UNPUBLISHED_FILES) {
        pipeline.consume(hash, payload).unwrap();
    }
    assert_eq!(pipeline.unpublished_files, MAX_UNPUBLISHED_FILES);
    assert_eq!(pipeline.progress.completed_entries, 0);
    assert_eq!(pipeline.progress.io.publication_batches, 0);
    assert!(pipeline.ready.is_empty());
    assert_eq!(
        pipeline.progress.io.peak_unpublished_files,
        MAX_UNPUBLISHED_FILES as u64
    );

    // No result could publish before the bound was reached. In the isolated
    // ready-file case, allow exactly one result while all remaining tasks are
    // either gated or still queued behind the two workers.
    first_ready.release();
    if pause_after_first_ready {
        pipeline.collect_verified(Duration::from_secs(5)).unwrap();
        assert_eq!(pipeline.ready.len(), 1);
        assert_eq!(
            pipeline.ready[0].pending.planned.manifest_index,
            first_ready_index
        );
        assert!(
            pipeline.verifying_files - pipeline.ready.len() > pipeline.verification.workers(),
            "the fixture must retain queued cohort work, not just a slow peer"
        );
        assert_eq!(
            pipeline.assembled.len(),
            MAX_UNPUBLISHED_FILES - PUBLICATION_BATCH
        );
        pipeline.tick().unwrap();
        assert_eq!(pipeline.progress.completed_entries, 0);
        assert_eq!(pipeline.progress.io.publication_batches, 0);
        assert_eq!(pipeline.progress.io.directory_syncs, 0);
        assert_eq!(pipeline.unpublished_files, MAX_UNPUBLISHED_FILES);
        assert_eq!(pipeline.ready.len(), 1);
        // The next input must wait at full capacity without publishing the one
        // ready file. Its progress callback then releases the fast cohort.
        arm_capacity_wait.store(true, Ordering::Release);
        pipeline.last_progress = None;
    } else {
        fast.release();
    }

    // Submit file 257 with every capacity slot occupied. The wait must coalesce
    // all fast files in the initial cohort, while the slow peer stays held.
    pipeline
        .consume(
            &hashes[MAX_UNPUBLISHED_FILES],
            &payloads[MAX_UNPUBLISHED_FILES],
        )
        .unwrap();
    assert_eq!(
        pipeline.progress.assembled_bytes,
        payloads
            .iter()
            .map(|payload| payload.len() as u64)
            .sum::<u64>()
    );
    assert_eq!(
        pipeline.progress.completed_entries,
        initial_fast.len() as u64
    );
    assert!(initial_fast.is_subset(&pipeline.transaction.published_entries));
    assert_eq!(pipeline.progress.io.publication_batches, 1);
    assert_eq!(pipeline.progress.io.partial_publication_batches, 1);
    assert_eq!(pipeline.progress.io.directory_syncs, 1);
    if pause_after_first_ready {
        assert!(saw_capacity_wait.load(Ordering::Acquire));
    }
    // The first cohort has published as one batch. Also require a real result
    // from the refill before releasing slow, not just a submission counter.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        pipeline.collect_verified(PUBLICATION_DELAY).unwrap();
        let returned = pipeline
            .transaction
            .published_entries
            .iter()
            .copied()
            .chain(
                pipeline
                    .ready
                    .iter()
                    .map(|verified| verified.pending.planned.manifest_index),
            )
            .collect::<BTreeSet<_>>();
        if initial_fast.is_subset(&returned)
            && pipeline.progress.io.verification_refill_batches > 0
            && returned.iter().any(|index| !initial_cohort.contains(index))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "initial fast cohort or a verified refill result remained stranded"
        );
        pipeline.tick().unwrap();
    }
    assert!(pipeline.progress.io.verification_refill_batches > 0);
    assert_eq!(pipeline.progress.io.publication_batches, 1);
    assert_eq!(pipeline.progress.io.directory_syncs, 1);
    assert_eq!(
        pipeline.progress.io.peak_unpublished_files,
        MAX_UNPUBLISHED_FILES as u64
    );
    assert!(pipeline.unpublished_files <= MAX_UNPUBLISHED_FILES);
    assert!(pipeline.verifying_files <= PUBLICATION_BATCH);
    assert!(pipeline.ready.len() <= pipeline.verifying_files);
    assert!(!slow_destination.exists());
    slow.release();
    let report = pipeline.finish().unwrap();
    transaction.commit().unwrap();
    assert_eq!(report.restore.published_entries, count);
    assert_eq!(report.io.file_syncs, count as u64);
    assert!(report.io.peak_unpublished_bytes <= MAX_UNPUBLISHED_BYTES);
    for (index, payload) in payloads.iter().enumerate() {
        assert_eq!(fs::read(game.join(index.to_string())).unwrap(), *payload);
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn publication_pause_blocks_new_file_syncs_and_cancellation_wakes_the_waiters() {
    for cancel in [false, true] {
        let root = test_dir("pipeline-sync-publication-gate");
        let payloads = [b"first".as_slice(), b"second"];
        let plan = test_plan(
            &root,
            test_manifest(vec![
                file("game/0", payloads[0]),
                file("game/1", payloads[1]),
            ]),
        );
        let roots = LogicalRootMap::from_home(root.join("home"));
        let game = root.join("home/.local/share/game");
        fs::create_dir_all(&game).unwrap();
        for index in 0..2 {
            fs::write(game.join(index.to_string()), b"original").unwrap();
        }
        let mut transaction = RestoreTransaction::new(&plan, &roots, None);
        let mut pipeline = Pipeline::new(&mut transaction, 2, |_| Ok(())).unwrap();
        pipeline.overlap_file_syncs = true;
        pipeline.publication_delay = Duration::MAX;
        for payload in payloads {
            pipeline
                .consume(&noland_cas::blake3_hex(payload), payload)
                .unwrap();
        }
        // Model a stalled directory barrier using the exact guard publication
        // owns, not an elapsed-time guess about whether a worker has started.
        let publication = pipeline.verification.pause_file_syncs().unwrap();
        pipeline.dispatch_assembled().unwrap();
        assert!(pipeline
            .verification
            .wait_for_paused_syncs(2, Duration::from_secs(5)));
        pipeline.collect_verified(Duration::ZERO).unwrap();
        pipeline.emit_progress(true).unwrap();
        assert!(pipeline.ready.is_empty());
        assert_eq!(pipeline.progress.io.file_syncs, 0);
        assert_eq!(pipeline.progress.completed_entries, 0);
        for index in 0..2 {
            assert_eq!(fs::read(game.join(index.to_string())).unwrap(), b"original");
        }
        if cancel {
            // This must join without requiring the caller to release the pause.
            // No blocked verifier may subsequently touch a rolled-back partial.
            drop(pipeline);
            drop(transaction);
            drop(publication);
            for index in 0..2 {
                assert_eq!(fs::read(game.join(index.to_string())).unwrap(), b"original");
            }
        } else {
            drop(publication);
            let report = pipeline.finish().unwrap();
            transaction.commit().unwrap();
            assert_eq!(report.io.file_syncs, 2);
            assert_eq!(report.restore.published_entries, 2);
            assert!(report.io.file_sync_publication_wait_us > 0);
            for (index, payload) in payloads.iter().enumerate() {
                assert_eq!(fs::read(game.join(index.to_string())).unwrap(), *payload);
            }
        }
        assert!(!plan.staging.exists());
        for index in 0..2 {
            assert!(
                !destination_partial_path(&game.join(index.to_string()), plan.restore_id)
                    .unwrap()
                    .exists()
            );
        }
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn a_file_changed_while_its_sync_is_paused_cannot_publish() {
    for mutation in ["contents", "mode", "inode"] {
        let root = test_dir("pipeline-paused-sync-tampering");
        let plan = test_plan(&root, test_manifest(vec![file("game/save", b"save")]));
        let roots = LogicalRootMap::from_home(root.join("home"));
        let destination = root.join("home/.local/share/game/save");
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::write(&destination, b"original").unwrap();
        let mut transaction = RestoreTransaction::new(&plan, &roots, None);
        let mut pipeline = Pipeline::new(&mut transaction, 1, |_| Ok(())).unwrap();
        pipeline.publication_delay = Duration::MAX;
        pipeline
            .consume(&noland_cas::blake3_hex(b"save"), b"save")
            .unwrap();
        let publication = pipeline.verification.pause_file_syncs().unwrap();
        pipeline.dispatch_assembled().unwrap();
        assert!(pipeline
            .verification
            .wait_for_paused_syncs(1, Duration::from_secs(5)));
        let partial = destination_partial_path(&destination, plan.restore_id).unwrap();
        match mutation {
            "contents" => fs::write(&partial, b"edit").unwrap(),
            "mode" => fs::set_permissions(&partial, fs::Permissions::from_mode(0o644)).unwrap(),
            "inode" => {
                fs::rename(&partial, root.join("old-partial")).unwrap();
                fs::write(&partial, b"save").unwrap();
                fs::set_permissions(&partial, fs::Permissions::from_mode(0o640)).unwrap();
            }
            _ => unreachable!(),
        }
        drop(publication);
        let error = pipeline.finish().unwrap_err();
        assert!(error
            .to_string()
            .contains("partial changed during verification"));
        drop(transaction);
        assert_eq!(fs::read(&destination).unwrap(), b"original");
        assert!(!partial.exists());
        assert!(!plan.staging.exists());
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn partial_cohort_publication_still_rolls_back_on_hash_failure_cancel_and_recovery() {
    for failure in ["hash", "cancel", "crash"] {
        let root = test_dir("pipeline-partial-cohort-rollback");
        let paths = AgentPaths::from_roots(root.join("state"), root.join("run"));
        paths.ensure_dirs().unwrap();
        let mut slow_file = file("game/0", b"slow");
        if failure == "hash" {
            slow_file.file_hash = noland_cas::blake3_hex(b"wrong full-file hash");
        }
        let plan = crash_test_plan(
            &paths,
            test_manifest(vec![slow_file, file("game/1", b"ready")]),
        );
        let roots = LogicalRootMap::from_home(root.join("home"));
        let game = root.join("home/.local/share/game");
        fs::create_dir_all(&game).unwrap();
        for index in 0..2 {
            fs::write(game.join(index.to_string()), format!("original-{index}")).unwrap();
            fs::set_permissions(
                game.join(index.to_string()),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        fs::write(game.join("unrelated"), b"keep-me").unwrap();
        let mut transaction = RestoreTransaction::new(&plan, &roots, None);
        let mut pipeline = Pipeline::new(&mut transaction, 2, |sample| {
            if failure == "cancel" && sample.completed_entries != 0 {
                return Err(StateError::Storage("cancel partial publication".into()));
            }
            Ok(())
        })
        .unwrap();
        pipeline.overlap_file_syncs = true;
        pipeline.publication_delay = Duration::MAX;
        let slow = Arc::new(Gate::default());
        let worker_slow = Arc::clone(&slow);
        let finished = Arc::new(AtomicBool::new(false));
        let worker_finished = Arc::clone(&finished);
        pipeline.verification = VerificationPool::start(2, move |task, stats, stopped, buffer| {
            if task.pending.planned.manifest_index == 0 {
                if let Err(error) = worker_slow.wait(stopped) {
                    // An in-flight syscall must finish BEFORE rollback/recovery
                    // removes its partial, including after partial publication.
                    assert!(task.pending.partial.is_file());
                    fs::write(&task.pending.partial, b"last-worker-write").unwrap();
                    worker_finished.store(true, Ordering::Release);
                    return Err(error);
                }
                let result = verification::verify_file(task, stats, stopped, buffer);
                worker_finished.store(true, Ordering::Release);
                return result;
            }
            verification::verify_file(task, stats, stopped, buffer)
        })
        .unwrap();
        for payload in [b"slow".as_slice(), b"ready"] {
            pipeline
                .consume(&noland_cas::blake3_hex(payload), payload)
                .unwrap();
        }
        pipeline.dispatch_assembled().unwrap();
        pipeline.collect_verified(Duration::from_secs(5)).unwrap();
        assert_eq!(pipeline.ready.len(), 1);
        pipeline.publication_delay = PUBLICATION_DELAY;
        pipeline.ready_since = Some(Instant::now() - PUBLICATION_DELAY);
        pipeline.last_progress = None;
        let result = pipeline.wait_for_publication(false);
        if failure == "cancel" {
            assert!(result.unwrap_err().to_string().contains("cancel partial"));
            drop(pipeline);
            drop(transaction);
        } else {
            result.unwrap();
            assert_eq!(pipeline.progress.completed_entries, 1);
            assert_eq!(pipeline.progress.io.partial_publication_batches, 1);
            assert_eq!(fs::read(game.join("1")).unwrap(), b"ready");
            assert_eq!(fs::read(game.join("0")).unwrap(), b"original-0");
            if failure == "hash" {
                slow.release();
                let error = pipeline.finish().unwrap_err();
                assert!(error
                    .to_string()
                    .contains("reconstructed file hash mismatch"));
                drop(transaction);
            } else {
                drop(pipeline);
                std::mem::forget(transaction);
                let report = crate::recover_interrupted_restores(&paths).unwrap();
                assert_eq!(report.workspaces_rolled_back, 1);
                assert!(report.failures.is_empty());
            }
        }
        assert!(finished.load(Ordering::Acquire), "workers must be joined");
        for index in 0..2 {
            let path = game.join(index.to_string());
            assert_eq!(
                fs::read(&path).unwrap(),
                format!("original-{index}").as_bytes()
            );
            assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, 0o600);
            assert!(!destination_partial_path(&path, plan.restore_id)
                .unwrap()
                .exists());
        }
        assert_eq!(fs::read(game.join("unrelated")).unwrap(), b"keep-me");
        assert!(!plan.staging.exists());
        fs::remove_dir_all(root).unwrap();
    }
}
