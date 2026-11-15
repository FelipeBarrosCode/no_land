use super::*;

fn tiny_fixture(
    files: usize,
) -> (
    PathBuf,
    StateAgent,
    MasterKey,
    Uuid,
    PreparedBackup,
    Provider,
) {
    let (root, agent, master, operation, mut prepared, provider) = fixture(files, 0);
    let old = prepared.capture.take().unwrap();
    let jobs = old
        .jobs
        .into_iter()
        .enumerate()
        .map(|(index, (job, _))| {
            // Empty files and duplicates cross the former 256-file receipt
            // boundaries. Source identities are frozen only after these writes.
            let bytes = if index % 5 == 0 {
                Vec::new()
            } else {
                format!("tiny-save-{}", index % 17).into_bytes()
            };
            std::fs::write(&job.source, bytes).unwrap();
            job
        })
        .collect();
    prepared.capture = Some(State::new_streaming(jobs, None, old.budget).unwrap());
    write_prepared_backup(&agent, &master, operation, &prepared).unwrap();
    (root, agent, master, operation, prepared, provider)
}

fn start_readers(
    agent: &StateAgent,
    master: &MasterKey,
    operation: Uuid,
    state: Arc<State>,
    stats: Arc<Counters>,
    readers: usize,
) -> CaptureWorkers {
    capture_workers(
        agent,
        operation,
        master,
        state,
        &prepared_backup_path(agent, operation).with_extension("stream"),
        stats,
        Arc::new(AtomicBool::new(false)),
        WorkerCeilings {
            readers,
            codecs: 1,
            capture_queue: readers.saturating_mul(2).max(1),
        },
    )
}

#[tokio::test]
async fn tiny_range_batches_durable_turns_and_replays_without_reopening_completed_sources() {
    let (root, agent, master, operation, mut prepared, mut provider) = tiny_fixture(BATCH_FILES);
    let expected = expected_sources(&prepared);
    let state = Arc::new(prepared.capture.take().unwrap());
    assert_eq!(state.ranges, [(0, BATCH_FILES)]);
    let source = state.jobs[0].0.source.clone();
    let stats = Arc::new(Counters::default());
    let mut readers = start_readers(&agent, &master, operation, state, stats.clone(), 1);
    let receipt = tokio::time::timeout(Duration::from_secs(120), readers.receiver.recv())
        .await
        .expect("tiny capture stalled")
        .unwrap()
        .unwrap();
    assert!(receipt.done);
    assert_eq!(receipt.files.len(), BATCH_FILES);
    assert_eq!(
        receipt.chunks.len(),
        expected.values().filter(|bytes| !bytes.is_empty()).count()
    );
    assert!(readers.receiver.recv().await.is_none());
    drop(readers);
    // Formerly a full tiny range needed four 256-file flushes and an empty
    // terminal receipt/checkpoint. Count real barrier/write call sites, not time.
    assert_eq!(stats.capture_receipts.load(Ordering::Relaxed), 1);
    assert_eq!(stats.capture_checkpoints.load(Ordering::Relaxed), 1);
    assert_eq!(stats.source_opens.load(Ordering::Relaxed), 2 * BATCH_FILES);
    assert_eq!(stats.active_readers.load(Ordering::Relaxed), 0);
    let directory = prepared_backup_path(&agent, operation).with_extension("stream");
    let path = directory.join("capture/0/0.enc");
    let frozen = std::fs::read(&path).unwrap();
    assert!(!directory.join("capture/0/1.enc").exists());

    // This is the cancellation/retry boundary after a complete durable receipt.
    // A newer source must survive, while the captured revision is replayed.
    std::fs::write(&source, b"newer save remains local").unwrap();
    let mut resumed = resume(&agent, &provider, &master, operation, &prepared);
    let (hooks, mut events) = hooks(HashMap::new(), HashMap::new());
    provider.hooks = Some(hooks.clone());
    provider.hold_uploads = true;
    let retry_stats = counters(hooks);
    let state = resumed.capture.take().unwrap();
    let execute = run_with_counters(
        &agent,
        &provider,
        &master,
        operation,
        &mut resumed,
        admission(),
        state,
        retry_stats.clone(),
    );
    let control = async {
        while let Some(event) = events.recv().await {
            if matches!(event, Event::Upload(_)) {
                assert_eq!(std::fs::read(&path).unwrap(), frozen);
                assert_eq!(retry_stats.source_opens.load(Ordering::Relaxed), 0);
                provider.release.add_permits(1024);
                return;
            }
        }
        panic!("tiny replay never reached its verified pack");
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(120), async {
        tokio::join!(execute, control)
    })
    .await
    .expect("tiny replay stalled");
    result.unwrap();
    assert_eq!(retry_stats.capture_receipts.load(Ordering::Relaxed), 0);
    assert_eq!(retry_stats.capture_checkpoints.load(Ordering::Relaxed), 0);
    assert_eq!(retry_stats.lease_validations.load(Ordering::Relaxed), 1);
    let hashes = expected
        .values()
        .filter(|bytes| !bytes.is_empty())
        .map(|bytes| noland_cas::blake3_hex(bytes))
        .collect::<HashSet<_>>();
    assert_eq!(resumed.pack_index.len(), hashes.len());
    assert!(hashes.len() < receipt.chunks.len());
    assert_restored_sources(&resumed, &master, &root.join("remote"), &expected);
    assert_eq!(std::fs::read(&source).unwrap(), b"newer save remains local");
    assert!(!directory.exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn empty_range_writes_one_authenticated_completion_without_a_second_cas_barrier() {
    let (root, agent, master, operation, mut prepared, _) = fixture(BATCH_FILES, 0);
    let state = Arc::new(prepared.capture.take().unwrap());
    let stats = Arc::new(Counters::default());
    let mut readers = start_readers(&agent, &master, operation, state, stats.clone(), 1);
    let receipt = tokio::time::timeout(Duration::from_secs(120), readers.receiver.recv())
        .await
        .expect("empty capture stalled")
        .unwrap()
        .unwrap();
    assert!(receipt.done);
    assert!(receipt.chunks.is_empty());
    assert_eq!(receipt.files.len(), BATCH_FILES);
    for (_, result) in &receipt.files {
        assert_eq!(result.manifest_file.file_hash, noland_cas::blake3_hex(b""));
        assert_eq!(result.manifest_file.size, 0);
        assert!(result.manifest_file.chunks.is_empty());
    }
    assert!(readers.receiver.recv().await.is_none());
    drop(readers);
    assert_eq!(stats.capture_receipts.load(Ordering::Relaxed), 1);
    // The initial durable lease receipt still exists. Nothing has been appended
    // to CAS since that barrier, so a metadata-only receipt need not seal again.
    assert_eq!(stats.capture_checkpoints.load(Ordering::Relaxed), 0);
    let directory = prepared_backup_path(&agent, operation).with_extension("stream");
    let replayed: Receipt = read(
        &directory.join("capture/0/0.enc"),
        &master,
        &format!("backup-stream:{operation}:capture:0:0"),
    )
    .unwrap()
    .unwrap();
    assert!(replayed.done && replayed.chunks.is_empty());
    assert_eq!(replayed.files.len(), BATCH_FILES);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn exact_segment_eof_receipt_keeps_its_prior_cas_barrier_and_large_file_prefix() {
    let (root, agent, master, operation, mut prepared, _) = fixture(1, SEGMENT_BYTES);
    let state = Arc::new(prepared.capture.take().unwrap());
    let stats = Arc::new(Counters::default());
    let mut readers = start_readers(&agent, &master, operation, state, stats.clone(), 1);
    let prefix = tokio::time::timeout(Duration::from_secs(120), readers.receiver.recv())
        .await
        .expect("large-file segment did not stream before EOF")
        .unwrap()
        .unwrap();
    assert!(!prefix.done);
    assert!(prefix.files.is_empty());
    assert_eq!(
        prefix
            .chunks
            .iter()
            .map(|chunk| chunk.source.len().unwrap())
            .sum::<u64>(),
        SEGMENT_BYTES
    );
    let complete = tokio::time::timeout(Duration::from_secs(120), readers.receiver.recv())
        .await
        .expect("file-only EOF receipt stalled")
        .unwrap()
        .unwrap();
    assert!(complete.done && complete.chunks.is_empty());
    assert_eq!(complete.files.len(), 1);
    assert_eq!(complete.files[0].1.manifest_file.size, SEGMENT_BYTES);
    assert_eq!(
        complete.files[0]
            .1
            .manifest_file
            .chunks
            .iter()
            .map(|chunk| chunk.hash.as_str())
            .collect::<Vec<_>>(),
        prefix
            .chunks
            .iter()
            .map(|chunk| chunk.hash.as_str())
            .collect::<Vec<_>>()
    );
    assert!(readers.receiver.recv().await.is_none());
    drop(readers);
    assert_eq!(stats.capture_receipts.load(Ordering::Relaxed), 2);
    assert_eq!(stats.capture_checkpoints.load(Ordering::Relaxed), 1);
    let mut reader = ChunkSourceReader::default();
    let mut bytes = Vec::new();
    for chunk in &prefix.chunks {
        reader
            .read_verified(&chunk.source, &chunk.hash, &mut bytes)
            .unwrap();
    }
    drop(reader);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn tiny_batch_rejects_a_source_replaced_after_read_before_any_completion_is_published() {
    let (root, agent, master, operation, mut prepared, provider) = tiny_fixture(64);
    let state = Arc::new(prepared.capture.take().unwrap());
    let source = state.jobs[7].0.source.clone();
    let gate = Arc::new(Gate::default());
    let (mut hooks, mut events) = hooks(HashMap::new(), HashMap::new());
    Arc::get_mut(&mut hooks)
        .unwrap()
        .sources
        .insert(7, gate.clone());
    let stats = counters(hooks);
    let mut readers = start_readers(&agent, &master, operation, state, stats.clone(), 1);
    tokio::time::timeout(Duration::from_secs(120), async {
        while !matches!(events.recv().await.unwrap(), Event::SourceRead(7)) {}
    })
    .await
    .expect("source-generation boundary was not reached");
    let previous = root.join("previous-save");
    std::fs::rename(&source, &previous).unwrap();
    std::fs::write(&source, b"replacement save must survive").unwrap();
    gate.release();
    let result = tokio::time::timeout(Duration::from_secs(120), readers.receiver.recv())
        .await
        .expect("changed source did not fail")
        .unwrap();
    assert!(matches!(result, Err(StateError::Conflict(_))));
    drop(readers);
    assert_eq!(stats.capture_receipts.load(Ordering::Relaxed), 0);
    assert_eq!(stats.capture_checkpoints.load(Ordering::Relaxed), 0);
    assert_eq!(stats.active_readers.load(Ordering::Relaxed), 0);
    assert_eq!(provider.remote_calls.load(Ordering::SeqCst), 0);
    assert!(!prepared_backup_path(&agent, operation)
        .with_extension("stream")
        .join("capture/0/0.enc")
        .exists());
    assert_eq!(
        std::fs::read(&source).unwrap(),
        b"replacement save must survive"
    );
    assert!(previous.is_file());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn cancellation_joins_an_unpublished_tiny_batch_and_retry_recaptures_only_unreceipted_work() {
    let (root, agent, master, operation, mut prepared, provider) = tiny_fixture(64);
    let expected = expected_sources(&prepared);
    let state = Arc::new(prepared.capture.take().unwrap());
    let gate = Arc::new(Gate::default());
    let (mut hooks, mut events) = hooks(HashMap::new(), HashMap::new());
    Arc::get_mut(&mut hooks)
        .unwrap()
        .sources
        .insert(31, gate.clone());
    let stats = counters(hooks);
    let readers = start_readers(&agent, &master, operation, state, stats.clone(), 1);
    tokio::time::timeout(Duration::from_secs(120), async {
        while !matches!(events.recv().await.unwrap(), Event::SourceRead(31)) {}
    })
    .await
    .expect("pending tiny batch was not reached");
    drop(readers); // Stop, close the queue and join the gated producer.
    assert!(!*gate.open.lock().unwrap());
    assert_eq!(stats.active_readers.load(Ordering::Relaxed), 0);
    assert_eq!(stats.capture_receipts.load(Ordering::Relaxed), 0);
    let directory = prepared_backup_path(&agent, operation).with_extension("stream");
    assert!(directory.join("capture/0/lease.enc").is_file());
    assert!(!directory.join("capture/0/0.enc").exists());
    let mut resumed = resume(&agent, &provider, &master, operation, &prepared);
    super::super::super::run(
        &agent,
        &provider,
        &master,
        operation,
        &mut resumed,
        admission(),
    )
    .await
    .unwrap();
    assert_restored_sources(&resumed, &master, &root.join("remote"), &expected);
    for (relative, bytes) in expected {
        let path = agent
            .config
            .home
            .join(".config/capture")
            .join(Path::new(&relative).file_name().unwrap());
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
    assert!(!directory.exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn short_range_reader_finishes_tiny_files_while_leading_large_ranges_are_held() {
    let files = BATCH_FILES + 2;
    let (root, agent, master, operation, mut prepared, _) = tiny_fixture(files);
    let old = prepared.capture.take().unwrap();
    let jobs = old
        .jobs
        .into_iter()
        .enumerate()
        .map(|(index, (job, _))| {
            if index < 2 {
                std::fs::File::create(&job.source)
                    .unwrap()
                    .set_len(2 * SEGMENT_BYTES)
                    .unwrap();
            }
            job
        })
        .collect();
    let state = Arc::new(State::new_streaming(jobs, None, old.budget).unwrap());
    let frozen = serde_json::to_value(&*state).unwrap();
    assert_eq!(state.ranges, [(0, 1), (1, 2), (2, files)]);
    let gates = [Arc::new(Gate::default()), Arc::new(Gate::default())];
    let (mut hooks, mut events) = hooks(HashMap::new(), HashMap::new());
    Arc::get_mut(&mut hooks).unwrap().captures =
        HashMap::from([(0, gates[0].clone()), (1, gates[1].clone())]);
    let stats = counters(hooks);
    let mut readers = start_readers(&agent, &master, operation, state.clone(), stats.clone(), 2);
    assert_eq!(readers.threads.len(), 2);
    let receipt = tokio::time::timeout(Duration::from_secs(120), async {
        let mut tiny = None;
        let mut large_started = false;
        loop {
            tokio::select! {
                result = readers.receiver.recv(), if tiny.is_none() => {
                    tiny = Some(result.unwrap().unwrap());
                }
                event = events.recv() => {
                    if matches!(event.unwrap(), Event::Capture(0 | 1)) {
                        large_started = true;
                    }
                }
            }
            if large_started {
                if let Some(receipt) = tiny.take() {
                    break receipt;
                }
            }
        }
    })
    .await
    .expect("large range claims occupied both readers ahead of the tiny range");
    assert!(receipt.done);
    assert_eq!(
        receipt
            .files
            .iter()
            .map(|(index, _)| *index)
            .collect::<Vec<_>>(),
        (2..files).collect::<Vec<_>>()
    );
    assert!(gates.iter().all(|gate| !*gate.open.lock().unwrap()));
    assert_eq!(serde_json::to_value(&*state).unwrap(), frozen);
    let directory = prepared_backup_path(&agent, operation).with_extension("stream");
    assert!(directory.join("capture/2/0.enc").is_file());
    assert!(!directory.join("capture/0/0.enc").exists());
    assert!(!directory.join("capture/1/0.enc").exists());
    drop(readers);
    assert_eq!(stats.active_readers.load(Ordering::Relaxed), 0);
    for (job, identity) in &state.jobs[..2] {
        assert_eq!(Identity::read(job).unwrap(), *identity);
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn admission_bounds_do_not_restat_legacy_sources_but_missing_inputs_still_fail_before_seal() {
    let (root, agent, master, operation, prepared, provider) = fixture(1, 128);
    let cas = LocalCas::new(agent.config.paths.cache.join("cas/chunks")).unwrap();
    let lease = cas.lease().unwrap();
    let bytes = b"legacy independently leased chunk";
    let hash = noland_cas::blake3_hex(bytes);
    let stored = lease.put_prehashed(&hash, bytes).unwrap();
    lease.checkpoint().unwrap();
    let queued = QueuedPlan::new(Plan {
        index: 0,
        chunks: vec![CapturedChunk {
            file: 0,
            hash,
            source: stored.path.clone().into(),
            lease: lease.root().to_path_buf(),
            small_save: true,
        }],
    })
    .unwrap();
    let bound = bytes.len() as u64 + 24 + scheduler::CHUNK_RECORD_BYTES;
    std::fs::remove_file(stored.path).unwrap();
    assert!(queued.plan.bytes().is_err());
    for _ in 0..BATCH_FILES {
        // Any metadata access here would fail. This estimate is scheduling
        // bookkeeping only; it must not authorize use of a missing source.
        assert_eq!(queued.bytes, bound);
    }
    let directory = prepared_backup_path(&agent, operation).with_extension("stream");
    let output = agent
        .config
        .paths
        .packs
        .join(prepared.manifest.bundle_id.to_string());
    let mut workers = pack_workers(
        &agent,
        &master,
        operation,
        &directory,
        &output,
        1,
        1,
        Arc::new(Counters::default()),
        Arc::new(AtomicBool::new(false)),
    );
    assert!(workers
        .sender
        .as_ref()
        .unwrap()
        .try_send(queued.plan)
        .is_ok());
    let result = tokio::time::timeout(Duration::from_secs(120), workers.receiver.recv())
        .await
        .expect("missing source did not fail")
        .unwrap();
    assert!(result.is_err());
    drop(workers);
    assert!(!directory.join("sealed/0.enc").exists());
    assert_eq!(provider.remote_calls.load(Ordering::SeqCst), 0);
    std::fs::remove_dir_all(root).unwrap();
}
