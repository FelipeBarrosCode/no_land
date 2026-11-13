use super::*;
use async_trait::async_trait;
use noland_storage::{Health, RemoteEntry, RemoteKey, RemoteMeta};
use std::io::Write;

struct Provider {
    inner: LocalStorage,
    calls: AtomicUsize,
    fail: AtomicBool,
    slow_first: bool,
    release: tokio::sync::Semaphore,
    active: AtomicUsize,
    peak: AtomicUsize,
    uploaded: Mutex<Vec<(String, String)>>,
}
#[async_trait]
impl SharedStorageProvider for Provider {
    async fn health_check(&self) -> Result<Health> {
        self.inner.health_check().await
    }
    async fn ensure_root(&self) -> Result<()> {
        self.inner.ensure_root().await
    }
    async fn stat(&self, key: &RemoteKey) -> Result<Option<RemoteMeta>> {
        self.inner.stat(key).await
    }
    async fn download(&self, key: &RemoteKey, path: &Path) -> Result<()> {
        self.inner.download(key, path).await
    }
    async fn list_prefix(&self, key: &RemoteKey) -> Result<Vec<RemoteEntry>> {
        self.inner.list_prefix(key).await
    }
    async fn put_small_versioned(&self, _: bytes::Bytes, _: &RemoteKey) -> Result<RemoteMeta> {
        panic!("stream capture cannot publish a manifest")
    }
    fn storage_identity(&self) -> Option<ProviderRootIdentity> {
        self.inner.storage_identity()
    }
    fn supports_targeted_uploads(&self) -> bool {
        true
    }
    fn transfer_tuning(&self) -> TransferTuning {
        TransferTuning {
            max_parallel_uploads: 2,
            max_bulk_files: 2,
            max_bulk_bytes: 20 << 20,
            max_attempts: 1,
            ..Default::default()
        }
    }
    async fn upload_immutable(&self, local: &Path, key: &RemoteKey) -> Result<RemoteMeta> {
        struct Active<'a>(&'a AtomicUsize);
        impl Drop for Active<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(active, Ordering::SeqCst);
        let _active = Active(&self.active);
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        self.uploaded
            .lock()
            .unwrap()
            .push((key.as_str().into(), noland_cas::blake3_file(local)?));
        if self.fail.load(Ordering::SeqCst) {
            return Err(StateError::Storage("injected transfer interruption".into()));
        }
        if self.slow_first && call == 1 {
            tokio::time::timeout(Duration::from_secs(20), self.release.acquire())
                .await
                .map_err(|_| StateError::Storage("straggler prevented slot refill".into()))?
                .unwrap()
                .forget();
        } else if call >= 3 {
            self.release.add_permits(1);
        }
        self.inner.upload_immutable(local, key).await
    }
}

fn fixture(
    files: usize,
    length: u64,
) -> (
    PathBuf,
    StateAgent,
    MasterKey,
    Uuid,
    PreparedBackup,
    Provider,
) {
    let (root, agent, master, operation, mut prepared) = super::super::tests::fixture(files);
    let jobs = prepared
        .capture
        .take()
        .unwrap()
        .jobs
        .into_iter()
        .map(|(job, _)| job)
        .collect::<Vec<_>>();
    for (i, job) in jobs.iter().enumerate() {
        let mut file = std::fs::File::create(&job.source).unwrap();
        // Distinct long runs produce bounded deterministic chunks and cheap
        // ciphertext. The sparse tail makes the pre-EOF fixture genuinely >2 GiB.
        let mut remaining = length.min(192 << 20);
        let mut block = 0u8;
        while remaining > 0 {
            let n = remaining.min(8 << 20) as usize;
            file.write_all(&vec![block.wrapping_add(i as u8); n])
                .unwrap();
            block = block.wrapping_add(1);
            remaining -= n as u64;
        }
        file.set_len(length).unwrap();
    }
    prepared.version = 3;
    prepared.capture = Some(
        State::new_streaming(
            jobs,
            None,
            preparation::Budget {
                hashing: 2,
                packing: 1,
            },
        )
        .unwrap(),
    );
    write_prepared_backup(&agent, &master, operation, &prepared).unwrap();
    let provider = Provider {
        inner: LocalStorage::new(root.join("remote")),
        calls: 0.into(),
        fail: false.into(),
        slow_first: false,
        release: tokio::sync::Semaphore::new(0),
        active: 0.into(),
        peak: 0.into(),
        uploaded: Mutex::default(),
    };
    (root, agent, master, operation, prepared, provider)
}

fn resume(
    agent: &StateAgent,
    provider: &Provider,
    master: &MasterKey,
    operation: Uuid,
    prepared: &PreparedBackup,
) -> PreparedBackup {
    read_prepared_backup(
        agent,
        provider,
        master,
        operation,
        &prepared.manifest.app.app_id,
        prepared.manifest.mode,
    )
    .unwrap()
    .unwrap()
}

#[tokio::test]
async fn upload_starts_before_multi_gigabyte_eof_and_changed_partial_revision_is_rejected() {
    let (root, agent, master, operation, mut prepared, provider) = fixture(1, (2 << 30) + 1);
    let source = prepared.capture.as_ref().unwrap().jobs[0].0.source.clone();
    provider.fail.store(true, Ordering::SeqCst);
    let failure = tokio::time::timeout(
        Duration::from_secs(30),
        super::super::run(&agent, &provider, &master, operation, &mut prepared),
    )
    .await
    .unwrap();
    assert!(failure.is_err());
    assert!(provider.calls.load(Ordering::SeqCst) > 0);
    assert_eq!(provider.active.load(Ordering::SeqCst), 0);
    let capture = prepared_backup_path(&agent, operation)
        .with_extension("stream")
        .join("capture/0");
    let mut captured = 0;
    let mut sequence = 0;
    while let Some(receipt) = read::<Receipt>(
        &capture.join(format!("{sequence}.enc")),
        &master,
        &format!("backup-stream:{operation}:capture:0:{sequence}"),
    )
    .unwrap()
    {
        assert!(!receipt.done, "upload waited for whole-file capture");
        captured += receipt
            .chunks
            .iter()
            .map(|c| c.source.len().unwrap())
            .sum::<u64>();
        sequence += 1;
    }
    assert!(captured > 0 && captured < 2 << 30);
    std::fs::write(&source, b"new generation must survive").unwrap();
    let mut resumed = resume(&agent, &provider, &master, operation, &prepared);
    // No old/new source concatenation is permitted, even though prefix packs exist.
    let state = resumed.capture.take().unwrap();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(64);
    let error = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                capture_work(
                    &agent.config.paths.cache.join("cas/chunks"),
                    &prepared_backup_path(&agent, operation).with_extension("stream"),
                    operation,
                    0,
                    &state,
                    &master,
                    &Counters::default(),
                    &|| false,
                    &sender,
                )
            })
            .join()
            .unwrap()
    })
    .unwrap_err();
    assert!(matches!(error, StateError::Conflict(_)));
    receiver.close();
    assert_eq!(
        std::fs::read(source).unwrap(),
        b"new generation must survive"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn independent_slots_resume_frozen_ciphertext_and_restore_exact_files() {
    let (root, agent, master, operation, mut prepared, mut provider) = fixture(2, 72 << 20);
    let expected = prepared
        .capture
        .as_ref()
        .unwrap()
        .jobs
        .iter()
        .map(|(job, _)| {
            (
                job.logical.relative_path.clone(),
                noland_cas::blake3_file(&job.source).unwrap(),
            )
        })
        .collect::<HashMap<_, _>>();
    provider.fail.store(true, Ordering::SeqCst);
    assert!(
        super::super::run(&agent, &provider, &master, operation, &mut prepared)
            .await
            .is_err()
    );
    let failed = provider.uploaded.lock().unwrap().clone();
    assert!(!failed.is_empty());
    provider.fail.store(false, Ordering::SeqCst);
    provider.calls.store(0, Ordering::SeqCst);
    provider.slow_first = true;
    let mut resumed = resume(&agent, &provider, &master, operation, &prepared);
    super::super::run(&agent, &provider, &master, operation, &mut resumed)
        .await
        .unwrap();
    assert_eq!(
        provider.peak.load(Ordering::SeqCst),
        2,
        "one codec must feed both independent upload slots"
    );
    assert_eq!(provider.active.load(Ordering::SeqCst), 0);
    for (key, checksum) in failed {
        let calls = provider.uploaded.lock().unwrap();
        let matches = calls
            .iter()
            .filter(|(candidate, _)| candidate == &key)
            .collect::<Vec<_>>();
        assert!(matches.len() >= 2);
        assert!(matches.iter().all(|(_, actual)| actual == &checksum));
    }
    assert_eq!(resumed.manifest.files.len(), 2);
    let progress = agent.db.get_operation_progress(operation).unwrap().unwrap();
    let pipeline = &progress.detail_json["streaming_pipeline"];
    assert!(pipeline["peak_reserved_bytes"].as_u64().unwrap() <= 20 << 20);
    assert_eq!(pipeline["active_readers"], 0);
    assert_eq!(pipeline["active_codecs"], 0);
    assert_eq!(pipeline["selected_files"], 2);
    assert_eq!(pipeline["read_bytes"], 144 << 20);
    assert_eq!(pipeline["durable_bytes"], 144 << 20);
    assert_eq!(pipeline["reserved_bytes"], 0);
    assert_eq!(pipeline["ready_packs"], 0);
    assert_eq!(pipeline["retained_capture_logical_bytes"], 0);
    assert_eq!(pipeline["waiting_on"], "manifest_commit");
    let mut buffer = Vec::new();
    for file in &resumed.manifest.files {
        buffer.clear();
        for chunk in &file.chunks {
            let entry = resumed
                .pack_index
                .iter()
                .find(|entry| entry.chunk_hash == chunk.hash)
                .unwrap();
            let bytes = noland_pack::extract_chunk(
                &root.join("remote").join(pack_key(&entry.pack_id)),
                entry,
                &master,
            )
            .unwrap();
            buffer.extend(bytes);
        }
        assert_eq!(buffer.len() as u64, file.size);
        assert_eq!(noland_cas::blake3_hex(&buffer), file.file_hash);
        assert_eq!(expected.get(&file.relative_path), Some(&file.file_hash));
    }
    assert!(!prepared_backup_path(&agent, operation)
        .with_extension("stream")
        .exists());
    assert!(
        std::fs::read_dir(agent.config.paths.cache.join("cas/chunks/.leases"))
            .unwrap()
            .next()
            .is_none()
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn authenticated_receipt_corruption_cannot_be_recaptured_or_committed() {
    let (root, agent, master, operation, mut prepared, provider) = fixture(1, 40 << 20);
    provider.fail.store(true, Ordering::SeqCst);
    assert!(
        super::super::run(&agent, &provider, &master, operation, &mut prepared)
            .await
            .is_err()
    );
    let receipt = prepared_backup_path(&agent, operation)
        .with_extension("stream")
        .join("capture/0/0.enc");
    let mut bytes = std::fs::read(&receipt).unwrap();
    let end = bytes.len() - 1;
    bytes[end] ^= 1;
    std::fs::write(&receipt, &bytes).unwrap();
    let mut resumed = resume(&agent, &provider, &master, operation, &prepared);
    provider.fail.store(false, Ordering::SeqCst);
    assert!(
        super::super::run(&agent, &provider, &master, operation, &mut resumed)
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(&receipt).unwrap(), bytes);
    bytes[end] ^= 1;
    std::fs::write(&receipt, &bytes).unwrap();
    let lease_receipt = receipt.with_file_name("lease.enc");
    let lease_bytes = std::fs::read(&lease_receipt).unwrap();
    let calls = provider.calls.load(Ordering::SeqCst);
    for replacement in [None, Some(b"corrupt lease".as_slice())] {
        std::fs::remove_file(&lease_receipt).unwrap();
        if let Some(bytes) = replacement {
            std::fs::write(&lease_receipt, bytes).unwrap();
        }
        let mut resumed = resume(&agent, &provider, &master, operation, &prepared);
        assert!(
            super::super::run(&agent, &provider, &master, operation, &mut resumed)
                .await
                .is_err()
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), calls);
        assert_eq!(std::fs::read(&receipt).unwrap(), bytes);
        // Keep the authenticated identity available for the next failure case.
        std::fs::write(&lease_receipt, &lease_bytes).unwrap();
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn cancellation_at_durable_handoff_replays_prefix_and_joins_every_writer() {
    let (root, agent, master, operation, mut prepared, provider) = fixture(1, 80 << 20);
    let state = prepared.capture.take().unwrap();
    let expected = noland_cas::blake3_file(&state.jobs[0].0.source).unwrap();
    let directory = prepared_backup_path(&agent, operation).with_extension("stream");
    let cas_root = agent.config.paths.cache.join("cas/chunks");
    let stop = Arc::new(AtomicBool::new(false));
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    let stopped = stop.clone();
    let key = master.clone();
    let thread = std::thread::spawn(move || {
        capture_work(
            &cas_root,
            &directory,
            operation,
            0,
            &state,
            &key,
            &Counters::default(),
            &|| stopped.load(Ordering::Acquire),
            &sender,
        )
    });
    let first = receiver.recv().await.unwrap().unwrap();
    assert!(!first.done && first.files.is_empty());
    assert!(
        first
            .chunks
            .iter()
            .map(|chunk| chunk.source.len().unwrap())
            .sum::<u64>()
            <= SEGMENT_BYTES
    );
    stop.store(true, Ordering::Release);
    receiver.close();
    assert!(thread.join().unwrap().is_err());
    let mut resumed = resume(&agent, &provider, &master, operation, &prepared);
    super::super::run(&agent, &provider, &master, operation, &mut resumed)
        .await
        .unwrap();
    assert_eq!(resumed.manifest.files[0].file_hash, expected);
    let mut hasher = blake3::Hasher::new();
    let mut length = 0u64;
    for chunk in &resumed.manifest.files[0].chunks {
        let entry = resumed
            .pack_index
            .iter()
            .find(|entry| entry.chunk_hash == chunk.hash)
            .unwrap();
        let bytes = noland_pack::extract_chunk(
            &root.join("remote").join(pack_key(&entry.pack_id)),
            entry,
            &master,
        )
        .unwrap();
        length += bytes.len() as u64;
        hasher.update(&bytes);
    }
    assert_eq!(length, 80 << 20);
    assert_eq!(format!("blake3:{}", hasher.finalize().to_hex()), expected);
    assert!(!prepared_backup_path(&agent, operation)
        .with_extension("stream")
        .exists());
    std::fs::remove_dir_all(root).unwrap();
}
