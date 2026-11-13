//! Durable partial captures. One next capture overlaps a count/byte-bounded
//! rolling set of sealing/upload batches. No partial bundle is
//! committed, and retries never recapture a batch that has a durable receipt.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
mod streaming;

const BATCH_FILES: usize = 1024;
const BATCH_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Identity {
    size: u64,
    mtime: Option<i64>,
    #[cfg(unix)]
    unix: (u64, u64, i64, i64),
}
impl Identity {
    fn read(job: &HashJob) -> Result<Self> {
        let file = open_regular_nofollow(job.staged.as_deref().unwrap_or(&job.source))?;
        let meta = file.metadata()?;
        Ok(Self::from_metadata(&meta))
    }

    fn from_metadata(meta: &std::fs::Metadata) -> Self {
        Self {
            size: meta.len(),
            mtime: SourceMetadata::from(meta).mtime_ns(),
            #[cfg(unix)]
            unix: (meta.dev(), meta.ino(), meta.ctime(), meta.ctime_nsec()),
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct State {
    #[serde(default)]
    streaming: bool,
    jobs: Vec<(HashJob, Identity)>,
    ranges: Vec<(usize, usize)>,
    snapshot: Option<PathBuf>,
    budget: preparation::Budget,
}
impl State {
    fn validate(&self) -> Result<()> {
        let mut next = 0;
        for &(start, end) in &self.ranges {
            if start != next || end <= start || end > self.jobs.len() || end - start > BATCH_FILES {
                return Err(StateError::Integrity(
                    "invalid partial capture ranges".into(),
                ));
            }
            next = end;
        }
        if next != self.jobs.len()
            || self.budget.hashing == 0
            || self.budget.hashing > 32
            || self.budget.packing == 0
            || self.budget.packing > 32
        {
            return Err(StateError::Integrity(
                "invalid partial capture worker budget".into(),
            ));
        }
        Ok(())
    }
    pub fn new(
        mut jobs: Vec<HashJob>,
        snapshot: Option<PathBuf>,
        budget: preparation::Budget,
    ) -> Result<Self> {
        // Stable path order preserves chunk locality across batches and retries.
        jobs.sort_by(|a, b| a.source.cmp(&b.source));
        let jobs = jobs
            .into_iter()
            .map(|job| Ok((Identity::read(&job)?, job)))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .map(|(identity, job)| (job, identity))
            .collect::<Vec<_>>();
        let mut ranges = Vec::new();
        let mut start = 0;
        while start < jobs.len() {
            let mut end = start;
            let mut bytes = 0u64;
            while end < jobs.len() && end - start < BATCH_FILES {
                let size = jobs[end].1.size;
                if end > start && bytes.saturating_add(size) > BATCH_BYTES {
                    break;
                }
                bytes = bytes.saturating_add(size);
                end += 1;
            }
            ranges.push((start, end));
            start = end;
        }
        Ok(Self {
            streaming: false,
            jobs,
            ranges,
            snapshot,
            budget,
        })
    }

    pub fn new_streaming(
        jobs: Vec<HashJob>,
        snapshot: Option<PathBuf>,
        budget: preparation::Budget,
    ) -> Result<Self> {
        let mut state = Self::new(jobs, snapshot, budget)?;
        state.streaming = true;
        state.ranges.clear();
        let mut start = 0;
        while start < state.jobs.len() {
            let mut end = start;
            let mut bytes = 0u64;
            while end < state.jobs.len() && end - start < BATCH_FILES {
                let size = state.jobs[end].1.size;
                if end > start && bytes.saturating_add(size) > 32 * 1024 * 1024 {
                    break;
                }
                bytes = bytes.saturating_add(size);
                end += 1;
            }
            state.ranges.push((start, end));
            start = end;
        }
        Ok(state)
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Batch {
    range: (usize, usize),
    lease: PathBuf,
    results: Vec<HashResult>,
    capture_ms: u64,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct Cleanup {
    pub(super) leases: Vec<PathBuf>,
    snapshot: Option<PathBuf>,
}

fn batch_path(agent: &StateAgent, operation: Uuid, index: usize) -> PathBuf {
    prepared_backup_path(agent, operation)
        .with_extension("capture")
        .join(format!("{index}.enc"))
}

fn write_batch(
    path: &Path,
    aad: &str,
    master: &MasterKey,
    batch: &impl serde::Serialize,
) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().expect("batch parent");
    std::fs::create_dir_all(parent)?;
    let bytes = noland_storage::seal_metadata(
        &noland_crypto::derive_keys(master).manifest,
        aad.as_bytes(),
        batch,
    )?
    .envelope;
    disk::ensure_space(parent, bytes.len() as u64)?;
    let partial = path.with_extension("partial");
    let mut file = std::fs::File::create(&partial)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    std::fs::rename(partial, path)?;
    // Persist all V3 directory levels beneath the already durable packs root.
    for directory in parent
        .ancestors()
        .take(4)
        .filter(|p| !p.as_os_str().is_empty())
    {
        std::fs::File::open(directory)?.sync_all()?;
    }
    Ok(())
}

struct Capture {
    receiver: tokio::sync::oneshot::Receiver<Result<Batch>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Capture {
    #[cfg(test)]
    async fn finish(mut self) -> Result<Batch> {
        let result = (&mut self.receiver)
            .await
            .map_err(|_| StateError::Storage("capture worker stopped".into()))?;
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| StateError::Storage("capture worker panicked".into()))?;
        }
        result
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.receiver.close();
        // Workers join before snapshots or leases can be released on cancellation.
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn start(
    agent: &StateAgent,
    master: &MasterKey,
    operation: Uuid,
    state: &State,
    index: usize,
    inherited: std::collections::HashSet<String>,
    binding: Option<String>,
) -> Result<Capture> {
    let range = state.ranges[index];
    let jobs = state.jobs[range.0..range.1].to_vec();
    let path = batch_path(agent, operation, index);
    let aad = format!("backup-capture:{operation}:{index}");
    let cas_root = agent.config.paths.cache.join("cas/chunks");
    let db = agent.db.clone();
    let master = master.clone();
    let operations = agent.operations.clone();
    let workers = state.budget.hashing;
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let thread = std::thread::spawn(move || {
        let result = (|| {
            let started = Instant::now();
            match std::fs::read(&path) {
                Ok(bytes) => {
                    let batch: Batch = noland_storage::open_metadata(
                        &noland_crypto::derive_keys(&master).manifest,
                        aad.as_bytes(),
                        bytes,
                    )?;
                    if batch.range != range || batch.results.len() != jobs.len() {
                        return Err(StateError::Integrity(
                            "partial capture range mismatch".into(),
                        ));
                    }
                    return Ok(batch);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            for (job, identity) in &jobs {
                if &Identity::read(job)? != identity {
                    return Err(StateError::Conflict(
                        "uncaptured backup source changed; start a new backup revision".into(),
                    ));
                }
            }
            let cas = LocalCas::new(cas_root)?;
            let lease = Arc::new(cas.lease()?);
            let cancelled: CancellationCheck = Arc::new(move || {
                stopped.load(Ordering::Acquire) || operations.cancel_requested(operation)
            });
            let now = Utc::now();
            let remote = RemoteChunkIndex {
                entries: HashMap::new(),
                scoped: binding.map(|binding| {
                    scoped_index::ScopedIndex::new(db, binding, master.clone(), now)
                }),
            };
            let mut results = Vec::new();
            hash_files_parallel(
                jobs.iter().map(|(job, _)| job.clone()).collect(),
                workers,
                lease.clone(),
                inherited,
                remote,
                now,
                cancelled.clone(),
                |result| {
                    results.push(result);
                    Ok(())
                },
            )?;
            for (job, identity) in &jobs {
                if &Identity::read(job)? != identity {
                    return Err(StateError::Conflict(
                        "backup source changed during partial capture".into(),
                    ));
                }
            }
            if results.len() != jobs.len() || cancelled() {
                return Err(cancellation_error());
            }
            results.sort_by(|a, b| {
                (
                    &a.manifest_file.logical_root,
                    &a.manifest_file.relative_path,
                )
                    .cmp(&(
                        &b.manifest_file.logical_root,
                        &b.manifest_file.relative_path,
                    ))
            });
            lease.sync_all()?;
            let batch = Batch {
                range,
                lease: lease.root().to_path_buf(),
                results,
                capture_ms: elapsed_ms(started),
            };
            // Retain before the fallible write: a rename followed by a failed
            // directory sync must never leave a receipt pointing at removed data.
            lease.retain_for_retry();
            write_batch(&path, &aad, &master, &batch)?;
            Ok(batch)
        })();
        let _ = sender.send(result);
    });
    Ok(Capture {
        receiver,
        stop,
        thread: Some(thread),
    })
}

pub(super) async fn run(
    agent: &StateAgent,
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    operation: Uuid,
    prepared: &mut PreparedBackup,
) -> Result<()> {
    let Some(state) = prepared.capture.take() else {
        return cleanup(agent, operation, prepared);
    };
    state.validate()?;
    if state.streaming {
        return streaming::run(agent, provider, master, operation, prepared, state).await;
    }
    let binding = repository_cache_binding(provider.storage_identity(), master);
    let mut inherited = prepared
        .pack_index
        .iter()
        .map(|entry| entry.chunk_hash.clone())
        .collect::<std::collections::HashSet<_>>();
    let tuning = provider.transfer_tuning().normalized();
    let upload_slots = tuning
        .max_parallel_uploads
        .min(tuning.max_bulk_files)
        .clamp(1, 32);
    let max_batches = state
        .budget
        .packing
        .min(upload_slots)
        .min(state.ranges.len().max(1))
        .max(1);
    let per_batch_slots = (upload_slots / max_batches).max(1);
    let session =
        noland_storage::PackUploadSession::new(provider, Some(&agent.db), Some(operation));
    let mut uploads = noland_storage::RollingUploadWindow::new(max_batches, tuning.max_bulk_bytes);
    let mut capture = if state.ranges.is_empty() {
        None
    } else {
        Some(start(
            agent,
            master,
            operation,
            &state,
            0,
            inherited.clone(),
            binding.clone(),
        )?)
    };
    let mut next_index = 0;
    let mut pending: Option<(Batch, PreparedBackup, usize, u64)> = None;
    let mut leases = Vec::new();
    let mut completed_batches = 0;
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        if agent.operations.cancel_requested(operation) {
            return Err(cancellation_error());
        }
        if pending
            .as_ref()
            .is_some_and(|(_, _, count, bytes)| uploads.can_push_count(*count, *bytes))
        {
            let (batch, mut part, count, bytes) = pending.take().unwrap();
            let session = &session;
            uploads.push_count(count, bytes, async move {
                packing::pack_and_upload_in_session(
                    agent, provider, master, operation, &mut part, session,
                )
                .await?;
                Ok((batch, part))
            })?;
            if next_index < state.ranges.len() {
                capture = Some(start(
                    agent,
                    master,
                    operation,
                    &state,
                    next_index,
                    inherited.clone(),
                    binding.clone(),
                )?);
            }
        }
        if capture.is_none() && pending.is_none() && uploads.is_empty() {
            break;
        }
        tokio::select! {
        captured = async { (&mut capture.as_mut().expect("capture").receiver).await }, if capture.is_some() && pending.is_none() => {
        let batch = captured.map_err(|_| StateError::Storage("capture worker stopped".into()))??;
        drop(capture.take()); // Join even if the sender published before returning.
        let index = next_index;
        next_index += 1;
        let mut chunks = BTreeMap::new();
        let mut small = BTreeSet::new();
        for result in &batch.results {
            for (hash, source) in &result.new_chunks {
                if inherited.insert(hash.clone()) { chunks.insert(hash.clone(), source.clone()); }
            }
            small.extend(result.small_file_chunk_hashes.iter().cloned());
        }
        let reserved = chunks.values().try_fold(0u64, |sum, source| Ok::<_, StateError>(sum.saturating_add(source.len()?).saturating_add(76)))?.max(1);
        let oversized = reserved > tuning.max_bulk_bytes;
        let mut small_chunks = Vec::new();
        let mut regular_chunks = Vec::new();
        for result in &batch.results {
            for chunk in &result.manifest_file.chunks {
                if let Some(source) = chunks.remove(&chunk.hash) {
                    if small.contains(&chunk.hash) { small_chunks.push((chunk.hash.clone(), source)); }
                    else { regular_chunks.push((chunk.hash.clone(), source)); }
                }
            }
        }
        let plans = packing::plans(small_chunks, regular_chunks)?;
        let mut manifest = BundleManifest::new(prepared.manifest.app.clone(), prepared.manifest.source.clone(), prepared.manifest.mode);
        manifest.bundle_id = prepared.manifest.bundle_id;
        let part = PreparedBackup {
            version: 1, storage_id: prepared.storage_id.clone(), manifest,
            pack_index: Vec::new(), pack_files: Vec::new(), new_pack_entries: Vec::new(), cas_observations: Vec::new(),
            trusted_states: Vec::new(), pending_mutations: Vec::new(), captured_generation: prepared.captured_generation,
            incremental: 0, metrics: OperationMetrics::default(), checksums: BTreeMap::new(), release_lease: None,
            capture: None, release_capture: None,
            packing: Some(packing::PackingState { plans, lease: batch.lease.clone(), workers: if oversized { state.budget.packing } else { (state.budget.packing / max_batches).max(1) },
                receipt_prefix: format!("{index}-"), partial_capture: true, upload_slots: Some(if oversized { upload_slots } else { per_batch_slots }) }),
        };
        pending = Some((batch, part, if oversized { max_batches } else { 1 }, reserved.min(tuning.max_bulk_bytes)));
        }
        result = uploads.next(), if !uploads.is_empty() => {
        let (batch, part) = result.expect("active batch")?;
        prepared.incremental = prepared.incremental.saturating_add(part.incremental);
        prepared.metrics.bytes_packed += part.metrics.bytes_packed;
        prepared.metrics.packing_duration_ms += part.metrics.packing_duration_ms;
        prepared.metrics.upload_duration_ms += part.metrics.upload_duration_ms;
        prepared.checksums.extend(part.checksums);
        prepared.pack_files.extend(part.pack_files);
        prepared.pack_index.extend(part.pack_index);
        prepared.new_pack_entries.extend(part.new_pack_entries);
        leases.push(batch.lease);
        prepared.metrics.hashing_duration_ms += batch.capture_ms;
        for result in batch.results { apply_result(prepared, result); }
        completed_batches += 1;
        let mut progress = agent.db.get_operation_progress(operation)?.unwrap_or_else(|| OperationProgress::new("capture_and_upload", 0));
        progress.detail_json["capture_pipeline"] = serde_json::json!({ "completed_batches": completed_batches, "total_batches": state.ranges.len(), "total_files": state.jobs.len(), "prefetch_batches": usize::from(capture.is_some()), "batch_file_limit": BATCH_FILES, "batch_byte_limit": BATCH_BYTES, "hash_workers": state.budget.hashing, "pack_workers": state.budget.packing, "inflight_batches": uploads.len(), "inflight_batch_limit": max_batches, "inflight_reserved_bytes": uploads.bytes(), "inflight_byte_limit": tuning.max_bulk_bytes });
        agent.db.set_operation_progress(operation, Some(&progress))?;
        }
        _ = tick.tick() => {}
        }
    }
    drop(uploads);
    prepared.manifest.files.sort_by(|a, b| {
        (&a.logical_root, &a.relative_path).cmp(&(&b.logical_root, &b.relative_path))
    });
    prepared
        .pack_index
        .sort_by(|a, b| a.chunk_hash.cmp(&b.chunk_hash));
    prepared
        .pack_index
        .dedup_by(|a, b| a.chunk_hash == b.chunk_hash);
    let referenced = prepared
        .manifest
        .files
        .iter()
        .flat_map(|f| &f.chunks)
        .map(|c| &c.hash)
        .collect::<std::collections::HashSet<_>>();
    prepared
        .pack_index
        .retain(|entry| referenced.contains(&entry.chunk_hash));
    if prepared.manifest.mode == BackupMode::CompleteApplication
        && prepared.manifest.files.is_empty()
    {
        return Err(StateError::Invalid(
            "complete backup contains no application files".into(),
        ));
    }
    let restore_mode = restore_mode_for_backup(prepared.manifest.mode);
    noland_restore::embed_restore_plan(&mut prepared.manifest, restore_mode);
    prepared.release_capture = Some(Cleanup {
        leases,
        snapshot: state.snapshot,
    });
    write_prepared_backup(agent, master, operation, prepared)?;
    cleanup(agent, operation, prepared)
}

fn apply_result(prepared: &mut PreparedBackup, result: HashResult) {
    let metrics = &mut prepared.metrics;
    metrics.snapshot_duration_ms += result.snapshot_duration_ms;
    if result.consistency == ConsistencyKind::BestEffort {
        prepared.manifest.consistency = ConsistencyKind::BestEffort;
    }
    metrics.num_files_rehashed += 1;
    metrics.bytes_scanned += result.metrics.bytes_hashed;
    metrics.bytes_hashed += result.metrics.bytes_hashed;
    metrics.bytes_chunked += result.metrics.bytes_hashed;
    metrics.num_chunks_reused += result.metrics.chunks_reused;
    metrics.num_chunks_created += result.metrics.chunks_created;
    metrics.num_local_cas_hits += result.metrics.local_cas_hits;
    metrics.num_remote_index_hits += result.metrics.remote_index_hits;
    metrics.bytes_reused_local += result.metrics.bytes_reused_local;
    prepared.pack_index.extend(result.remote_entries);
    prepared.cas_observations.extend(result.cas_observations);
    prepared.trusted_states.push(result.file_state);
    prepared.manifest.files.push(result.manifest_file);
}

fn cleanup(agent: &StateAgent, operation: Uuid, prepared: &PreparedBackup) -> Result<()> {
    let Some(cleanup) = &prepared.release_capture else {
        return Ok(());
    };
    let cas = LocalCas::new(agent.config.paths.cache.join("cas/chunks"))?;
    for lease in &cleanup.leases {
        cas.release_lease(lease)?;
    }
    let max = std::env::var("NOLAND_CAS_CACHE_BYTES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1024 * 1024 * 1024);
    cas.evict_to(max)?;
    if let Some(snapshot) = &cleanup.snapshot {
        if snapshot.parent() != Some(agent.config.paths.snapshots.as_path()) {
            return Err(StateError::UnsafePath(
                "invalid partial capture snapshot".into(),
            ));
        }
        if snapshot.exists() {
            noland_snapshot::discard_root(snapshot)?;
        }
    }
    for extension in ["capture", "packs", "stream"] {
        let path = prepared_backup_path(agent, operation).with_extension(extension);
        match std::fs::remove_dir_all(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use noland_storage::{Health, RemoteEntry, RemoteKey, RemoteMeta};
    use std::sync::atomic::AtomicUsize;

    struct Probe {
        inner: LocalStorage,
        capture_dir: PathBuf,
        first: AtomicBool,
        uploads: AtomicUsize,
        fail: bool,
    }
    #[async_trait]
    impl SharedStorageProvider for Probe {
        async fn health_check(&self) -> Result<Health> {
            self.inner.health_check().await
        }
        async fn ensure_root(&self) -> Result<()> {
            self.inner.ensure_root().await
        }
        async fn stat(&self, key: &RemoteKey) -> Result<Option<RemoteMeta>> {
            self.inner.stat(key).await
        }
        async fn download(&self, key: &RemoteKey, dest: &Path) -> Result<()> {
            self.inner.download(key, dest).await
        }
        async fn list_prefix(&self, prefix: &RemoteKey) -> Result<Vec<RemoteEntry>> {
            self.inner.list_prefix(prefix).await
        }
        async fn put_small_versioned(
            &self,
            bytes: bytes::Bytes,
            key: &RemoteKey,
        ) -> Result<RemoteMeta> {
            self.inner.put_small_versioned(bytes, key).await
        }
        fn storage_identity(&self) -> Option<ProviderRootIdentity> {
            self.inner.storage_identity()
        }
        fn supports_targeted_uploads(&self) -> bool {
            true
        }
        async fn upload_immutable(&self, local: &Path, key: &RemoteKey) -> Result<RemoteMeta> {
            self.uploads.fetch_add(1, Ordering::SeqCst);
            if self.first.swap(false, Ordering::SeqCst) {
                // Hold the first upload while the next capture becomes durable.
                // A fourth batch cannot start: two active batches plus one prefetch.
                tokio::time::timeout(Duration::from_secs(10), async {
                    while !self.capture_dir.join("1.enc").is_file() {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                })
                .await
                .map_err(|_| StateError::Storage("next capture did not overlap upload".into()))?;
                assert!(!self.capture_dir.join("3.enc").exists());
                if self.fail {
                    return Err(StateError::Storage(
                        "interrupted while a future capture was durable".into(),
                    ));
                }
            }
            self.inner.upload_immutable(local, key).await
        }
    }

    pub(super) fn fixture(files: usize) -> (PathBuf, StateAgent, MasterKey, Uuid, PreparedBackup) {
        let root = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("partial-capture-{}", Uuid::new_v4()));
        let agent = StateAgent::boot(crate::AgentConfig::isolated(root.clone())).unwrap();
        let master = MasterKey::generate();
        let operation = Uuid::new_v4();
        let app = AppIdentity::new(AppId::desktop("partial-capture"), "Partial Capture");
        agent.db.upsert_app(&app).unwrap();
        let now = Utc::now();
        agent
            .db
            .upsert_operation(&OperationRecord {
                operation_id: operation,
                app_id: Some(app.app_id.clone()),
                kind: "backup".into(),
                state: "HASHING".into(),
                created_at: now,
                updated_at: now,
                last_error: None,
                detail_json: serde_json::json!({}),
            })
            .unwrap();
        let directory = agent.config.home.join(".config/capture");
        std::fs::create_dir_all(&directory).unwrap();
        let mut jobs = Vec::new();
        for index in 0..files {
            let source = directory.join(format!("save-{index:04}.dat"));
            // A duplicate on both sides of each batch boundary tests cross-batch
            // chunk references without recapturing/repacking earlier payloads.
            let bytes = format!("save-{}", index.div_ceil(2));
            std::fs::write(&source, bytes).unwrap();
            let logical = agent.roots.lock().classify(&source).unwrap();
            let record = PathRecord {
                path_id: index as i64 + 1,
                canonical_path: source.to_string_lossy().into_owned(),
                logical_root: Some(logical.logical_root.as_token()),
                relative_path: Some(logical.relative_path.clone()),
                file_type: Some("file".into()),
                inode: None,
                mount_id: None,
                size: None,
                mtime_ns: None,
                mode: None,
                uid: None,
                gid: None,
                content_hash: None,
                last_scanned_at: None,
            };
            let association = PathAssociation {
                app_id: app.app_id.clone(),
                path_id: record.path_id,
                confidence: 1.0,
                evidence: Vec::new(),
                persistence_class: PersistenceClass::PersistentState,
                semantic_role: SemanticRole::UserState,
                first_seen_at: now,
                last_seen_at: now,
            };
            jobs.push(HashJob {
                app_id: app.app_id.clone(),
                source,
                snapshot_root: agent.config.paths.snapshots.clone(),
                staged: None,
                record,
                association,
                logical,
                shared_app_ids: Vec::new(),
            });
        }
        let storage = LocalStorage::new(root.join("remote"));
        let prepared = PreparedBackup {
            version: 2,
            storage_id: storage.storage_identity().map(|i| i.cache_key()),
            manifest: BundleManifest::new(
                ManifestApp::from(&app),
                ManifestSource {
                    instance_id: agent.config.instance_id,
                    image_id: "fixture".into(),
                    captured_at: now,
                },
                BackupMode::PersonalState,
            ),
            pack_index: Vec::new(),
            pack_files: Vec::new(),
            new_pack_entries: Vec::new(),
            cas_observations: Vec::new(),
            trusted_states: Vec::new(),
            pending_mutations: Vec::new(),
            captured_generation: 0,
            incremental: 0,
            metrics: OperationMetrics::default(),
            checksums: BTreeMap::new(),
            packing: None,
            release_lease: None,
            capture: Some(
                State::new(
                    jobs,
                    None,
                    preparation::Budget {
                        hashing: 2,
                        packing: 2,
                    },
                )
                .unwrap(),
            ),
            release_capture: None,
        };
        write_prepared_backup(&agent, &master, operation, &prepared).unwrap();
        (root, agent, master, operation, prepared)
    }

    #[tokio::test]
    async fn interrupted_partial_capture_overlaps_upload_and_resumes_without_recapturing_saved_files(
    ) {
        let (root, agent, master, operation, mut prepared) = fixture(BATCH_FILES * 3 + 2);
        let provider = Probe {
            inner: LocalStorage::new(root.join("remote")),
            capture_dir: prepared_backup_path(&agent, operation).with_extension("capture"),
            first: true.into(),
            uploads: 0.into(),
            fail: true,
        };
        assert!(run(&agent, &provider, &master, operation, &mut prepared)
            .await
            .is_err());
        assert!(provider.capture_dir.join("0.enc").exists());
        assert!(provider.capture_dir.join("1.enc").exists());
        assert!(!provider.capture_dir.join("3.enc").exists());
        assert!(!root
            .join("remote")
            .join(bundle_dir(
                &prepared.manifest.app.app_id,
                prepared.manifest.bundle_id
            ))
            .join(COMMITTED_MARKER)
            .exists());
        let original = agent.config.home.join(".config/capture/save-0000.dat");
        std::fs::write(&original, b"newer save must survive retry").unwrap();
        let mut resumed = read_prepared_backup(
            &agent,
            &provider,
            &master,
            operation,
            &prepared.manifest.app.app_id,
            BackupMode::PersonalState,
        )
        .unwrap()
        .unwrap();
        run(&agent, &provider, &master, operation, &mut resumed)
            .await
            .unwrap();
        assert_eq!(resumed.manifest.files.len(), BATCH_FILES * 3 + 2);
        for file in &resumed.manifest.files {
            let entry = resumed
                .pack_index
                .iter()
                .find(|entry| entry.chunk_hash == file.chunks[0].hash)
                .unwrap();
            let path = root.join("remote").join(pack_key(&entry.pack_id));
            let payload = noland_pack::PackDecodeContext::new(&entry.pack_id, &master)
                .unwrap()
                .extract_chunk(&path, entry)
                .unwrap();
            assert_eq!(noland_cas::blake3_hex(&payload), file.file_hash);
        }
        assert_eq!(
            std::fs::read(original).unwrap(),
            b"newer save must survive retry"
        );
        assert_eq!(
            resumed.manifest.files[0].file_hash,
            noland_cas::blake3_hex(b"save-0")
        );
        assert!(!provider.capture_dir.exists());
        assert!(
            std::fs::read_dir(agent.config.paths.cache.join("cas/chunks/.leases"))
                .unwrap()
                .next()
                .is_none()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn changed_uncaptured_sources_fail_closed_and_corrupt_batch_cannot_be_recaptured() {
        let (root, agent, master, operation, mut prepared) = fixture(2);
        let state = prepared.capture.as_ref().unwrap();
        let source = state.jobs[0].0.source.clone();
        std::fs::write(&source, b"changed after durable selection").unwrap();
        let error = start(
            &agent,
            &master,
            operation,
            state,
            0,
            Default::default(),
            None,
        )
        .unwrap()
        .finish()
        .await
        .unwrap_err();
        assert!(matches!(error, StateError::Conflict(_)));
        assert!(!batch_path(&agent, operation, 0).exists());
        prepared.capture = Some(
            State::new(
                state.jobs.iter().map(|(job, _)| job.clone()).collect(),
                None,
                state.budget,
            )
            .unwrap(),
        );
        let state = prepared.capture.as_ref().unwrap();
        start(
            &agent,
            &master,
            operation,
            state,
            0,
            Default::default(),
            None,
        )
        .unwrap()
        .finish()
        .await
        .unwrap();
        let receipt = batch_path(&agent, operation, 0);
        let mut bytes = std::fs::read(&receipt).unwrap();
        let end = bytes.len() - 1;
        bytes[end] ^= 1;
        std::fs::write(&receipt, bytes).unwrap();
        assert!(start(
            &agent,
            &master,
            operation,
            state,
            0,
            Default::default(),
            None
        )
        .unwrap()
        .finish()
        .await
        .is_err());
        assert_eq!(
            std::fs::read(source).unwrap(),
            b"changed after durable selection"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
