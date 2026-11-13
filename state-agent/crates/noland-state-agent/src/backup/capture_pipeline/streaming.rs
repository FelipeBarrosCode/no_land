//! V3 capture protocol: immutable segment receipts feed an operation-wide pack
//! queue before file EOF. Only completed file receipts can enter the manifest.
use super::*;
use noland_cas::{ChunkSourceReader, ChunkStreamResult};
use std::collections::{HashSet, VecDeque};
use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicU64, AtomicUsize};
use std::sync::Mutex;

const SEGMENT_BYTES: u64 = 32 * 1024 * 1024;
const FIRST_PACK_BYTES: u64 = 64 * 1024 * 1024;
// Preserve the default until comparable benchmarks select a better target.
const PACK_BYTES: u64 = constants::PACK_TARGET;

#[cfg(test)]
mod tests;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct CapturedChunk {
    file: usize,
    hash: String,
    source: ChunkSource,
    lease: PathBuf,
    small_save: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Receipt {
    chunks: Vec<CapturedChunk>,
    files: Vec<(usize, HashResult)>,
    done: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Plan {
    index: usize,
    chunks: Vec<CapturedChunk>,
}
impl Plan {
    fn bytes(&self) -> Result<u64> {
        self.chunks.iter().try_fold(24u64, |n, c| {
            Ok(n.saturating_add(c.source.len()?).saturating_add(52))
        })
    }
    fn packing(&self) -> packing::PackPlan {
        packing::PackPlan {
            chunks: self
                .chunks
                .iter()
                .map(|c| (c.hash.clone(), c.source.clone()))
                .collect(),
            target: constants::PACK_MAX,
            max: constants::PACK_MAX,
        }
    }
}

fn read<T: serde::de::DeserializeOwned>(
    path: &Path,
    master: &MasterKey,
    aad: &str,
) -> Result<Option<T>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(noland_storage::open_metadata(
            &noland_crypto::derive_keys(master).manifest,
            aad.as_bytes(),
            bytes,
        )?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[derive(Default)]
struct Counters {
    read_bytes: AtomicU64,
    durable_bytes: AtomicU64,
    active_readers: AtomicUsize,
    active_codecs: AtomicUsize,
    capture_wait_us: AtomicU64,
}

/// Replays a verified local prefix, then reads only the still-uncaptured suffix
/// from the anchored source. This reconstructs the exact FastCDC/BLAKE3 state
/// without persisting implementation-specific hash internals or rereading the
/// live prefix. Every partial source identity is checked before and after EOF.
struct PrefixReader<'a> {
    file: &'a mut std::fs::File,
    prefix: VecDeque<CapturedChunk>,
    reader: ChunkSourceReader,
    bytes: Vec<u8>,
    offset: usize,
    counters: &'a Counters,
    cancelled: &'a dyn Fn() -> bool,
}
impl Read for PrefixReader<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if (self.cancelled)() {
            return Err(std::io::Error::other("capture cancelled"));
        }
        if self.offset == self.bytes.len() {
            if let Some(chunk) = self.prefix.pop_front() {
                self.reader
                    .read_verified(&chunk.source, &chunk.hash, &mut self.bytes)
                    .map_err(std::io::Error::other)?;
                self.offset = 0;
            }
        }
        let n = if self.offset < self.bytes.len() {
            let n = output.len().min(self.bytes.len() - self.offset);
            output[..n].copy_from_slice(&self.bytes[self.offset..self.offset + n]);
            self.offset += n;
            n
        } else {
            self.file.read(output)?
        };
        self.counters
            .read_bytes
            .fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

struct CaptureWorkers {
    receiver: tokio::sync::mpsc::Receiver<Result<Receipt>>,
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}
impl Drop for CaptureWorkers {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.receiver.close();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn capture_workers(
    agent: &StateAgent,
    operation: Uuid,
    master: &MasterKey,
    state: Arc<State>,
    directory: &Path,
    counters: Arc<Counters>,
    stop: Arc<AtomicBool>,
) -> CaptureWorkers {
    let workers = state.budget.hashing.min(4).min(state.ranges.len().max(1));
    let (sender, receiver) = tokio::sync::mpsc::channel(workers * 2);
    let next = Arc::new(AtomicUsize::new(0));
    let mut pool = CaptureWorkers {
        receiver,
        stop: stop.clone(),
        threads: Vec::new(),
    };
    for _ in 0..workers {
        let (sender, next, state, stop, counters) = (
            sender.clone(),
            next.clone(),
            state.clone(),
            stop.clone(),
            counters.clone(),
        );
        let directory = directory.to_path_buf();
        let cas_root = agent.config.paths.cache.join("cas/chunks");
        let master = master.clone();
        let operations = agent.operations.clone();
        pool.threads.push(std::thread::spawn(move || {
            let cancelled =
                || stop.load(Ordering::Acquire) || operations.cancel_requested(operation);
            while !cancelled() {
                let index = next.fetch_add(1, Ordering::Relaxed);
                if index >= state.ranges.len() {
                    break;
                }
                counters.active_readers.fetch_add(1, Ordering::Relaxed);
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    capture_work(
                        &cas_root, &directory, operation, index, &state, &master, &counters,
                        &cancelled, &sender,
                    )
                }))
                .unwrap_or_else(|_| Err(StateError::Storage("capture worker panicked".into())));
                counters.active_readers.fetch_sub(1, Ordering::Relaxed);
                if let Err(e) = result {
                    let _ = sender.blocking_send(Err(e));
                    break;
                }
            }
        }));
    }
    pool
}

#[allow(clippy::too_many_arguments)]
fn capture_work(
    cas_root: &Path,
    directory: &Path,
    operation: Uuid,
    work: usize,
    state: &State,
    master: &MasterKey,
    counters: &Counters,
    cancelled: &dyn Fn() -> bool,
    sender: &tokio::sync::mpsc::Sender<Result<Receipt>>,
) -> Result<()> {
    let range = state.ranges[work];
    let directory = directory.join("capture").join(work.to_string());
    let aad = |name: &str| format!("backup-stream:{operation}:capture:{work}:{name}");
    let cas = LocalCas::new(cas_root)?;
    let lease =
        if let Some(path) = read::<PathBuf>(&directory.join("lease.enc"), master, &aad("lease"))? {
            cas.resume_lease(&path)?
        } else {
            let lease = cas.lease()?;
            lease.checkpoint()?;
            lease.retain_for_retry();
            write_batch(
                &directory.join("lease.enc"),
                &aad("lease"),
                master,
                &lease.root(),
            )?;
            lease
        };
    let mut sequence = 0;
    let mut chunker = ChunkFileReader::default();
    let mut completed = HashSet::new();
    let mut prefix: HashMap<usize, Vec<CapturedChunk>> = HashMap::new();
    loop {
        let Some(receipt) = read::<Receipt>(
            &directory.join(format!("{sequence}.enc")),
            master,
            &aad(&sequence.to_string()),
        )?
        else {
            break;
        };
        sequence += 1;
        for chunk in &receipt.chunks {
            if chunk.file < range.0
                || chunk.file >= range.1
                || chunk.lease != lease.root()
                || completed.contains(&chunk.file)
            {
                return Err(StateError::Integrity(
                    "invalid streaming capture sequence".into(),
                ));
            }
            prefix.entry(chunk.file).or_default().push(chunk.clone());
            counters
                .durable_bytes
                .fetch_add(chunk.source.len()?, Ordering::Relaxed);
        }
        for (index, result) in &receipt.files {
            if *index < range.0 || *index >= range.1 || !completed.insert(*index) {
                return Err(StateError::Integrity(
                    "duplicate streaming file receipt".into(),
                ));
            }
            let chunks = prefix.remove(index).unwrap_or_default();
            if chunks.len() != result.manifest_file.chunks.len()
                || chunks
                    .iter()
                    .zip(&result.manifest_file.chunks)
                    .any(|(a, b)| a.hash != b.hash || a.source.len().ok() != Some(b.size))
            {
                return Err(StateError::Integrity(
                    "streaming file/chunk receipt mismatch".into(),
                ));
            }
            if result.manifest_file.size != state.jobs[*index].1.size
                || result.manifest_file.relative_path != state.jobs[*index].0.logical.relative_path
                || result.manifest_file.logical_root
                    != state.jobs[*index].0.logical.logical_root.as_token()
            {
                return Err(StateError::Integrity(
                    "streaming file identity mismatch".into(),
                ));
            }
            counters
                .read_bytes
                .fetch_add(result.manifest_file.size, Ordering::Relaxed);
        }
        let done = receipt.done;
        if done && (completed.len() != range.1 - range.0 || !prefix.is_empty()) {
            return Err(StateError::Integrity(
                "incomplete final streaming capture".into(),
            ));
        }
        sender
            .blocking_send(Ok(receipt))
            .map_err(|_| cancellation_error())?;
        if done {
            return Ok(());
        }
    }
    let mut pending = Receipt {
        chunks: Vec::new(),
        files: Vec::new(),
        done: false,
    };
    let mut pending_bytes = 0u64;
    let mut flush = |pending: &mut Receipt| -> Result<()> {
        if cancelled() {
            return Err(cancellation_error());
        }
        if pending.chunks.is_empty() && pending.files.is_empty() && !pending.done {
            return Ok(());
        }
        lease.checkpoint()?;
        write_batch(
            &directory.join(format!("{sequence}.enc")),
            &aad(&sequence.to_string()),
            master,
            pending,
        )?;
        sequence += 1;
        counters.durable_bytes.fetch_add(
            pending
                .chunks
                .iter()
                .map(|c| c.source.len().unwrap_or(0))
                .sum(),
            Ordering::Relaxed,
        );
        let wait = Instant::now();
        let receipt = std::mem::replace(
            pending,
            Receipt {
                chunks: Vec::new(),
                files: Vec::new(),
                done: false,
            },
        );
        sender
            .blocking_send(Ok(receipt))
            .map_err(|_| cancellation_error())?;
        counters
            .capture_wait_us
            .fetch_add(wait.elapsed().as_micros() as u64, Ordering::Relaxed);
        Ok(())
    };
    for index in range.0..range.1 {
        if completed.contains(&index) {
            continue;
        }
        let (job, identity) = &state.jobs[index];
        if &Identity::read(job)? != identity {
            return Err(StateError::Conflict(
                "uncaptured streaming source changed; start a new backup revision".into(),
            ));
        }
        let source = open_regular_nofollow(job.staged.as_deref().unwrap_or(&job.source))?;
        let metadata = source.metadata()?;
        if &Identity::from_metadata(&metadata) != identity {
            return Err(StateError::Conflict(
                "streaming source replaced before open".into(),
            ));
        }
        let old = prefix.remove(&index).unwrap_or_default();
        let offset = old
            .iter()
            .try_fold(0u64, |sum, c| Ok::<_, StateError>(sum + c.source.len()?))?;
        if offset > metadata.len() {
            return Err(StateError::Integrity(
                "capture prefix exceeds source size".into(),
            ));
        }
        let view = if job.staged.is_none() && !noland_cas::is_small_file(identity.size) {
            // Keep transient CoW artifacts beneath the operation-owned capture
            // tree so crash leftovers remain covered by retry cleanup too.
            noland_snapshot::create_cow_file_view(&directory.join("cow"), &job.source, &source)?
        } else {
            None
        };
        let _cleanup = view.as_ref().map(|view| SnapshotCleanup(view.root.clone()));
        let mut captured_source = match &view {
            Some(view) => open_regular_nofollow(&view.mappings[0].staged)?,
            None => source.try_clone()?,
        };
        let captured_identity = captured_source.metadata()?;
        captured_source.seek(SeekFrom::Start(offset))?;
        let started = Instant::now();
        let mut chunk_index = 0;
        let mut on_chunk = |chunk: &ChunkRef, payload: &[u8]| {
            if cancelled() {
                return Err(cancellation_error());
            }
            if let Some(previous) = old.get(chunk_index) {
                if previous.hash != chunk.hash || previous.source.len()? != chunk.size {
                    return Err(StateError::Integrity(
                        "captured prefix changed chunk boundaries".into(),
                    ));
                }
            } else {
                if pending_bytes > 0 && pending_bytes.saturating_add(chunk.size) > SEGMENT_BYTES {
                    flush(&mut pending)?;
                    pending_bytes = 0;
                }
                let stored = lease.capture_streaming_prehashed(&chunk.hash, payload)?;
                pending.chunks.push(CapturedChunk {
                    file: index,
                    hash: chunk.hash.clone(),
                    source: stored.source,
                    lease: lease.root().to_path_buf(),
                    small_save: noland_cas::is_small_file(identity.size)
                        && matches!(
                            job.association.semantic_role,
                            SemanticRole::UserState | SemanticRole::Secret
                        ),
                });
                pending_bytes += chunk.size;
                if pending_bytes >= SEGMENT_BYTES {
                    flush(&mut pending)?;
                    pending_bytes = 0;
                }
            }
            chunk_index += 1;
            Ok(())
        };
        let summary = if old.is_empty() && noland_cas::is_small_file(identity.size) {
            let summary = chunker.chunk_open_file(&mut captured_source, &mut on_chunk)?;
            counters
                .read_bytes
                .fetch_add(summary.size, Ordering::Relaxed);
            summary
        } else {
            let input = PrefixReader {
                file: &mut captured_source,
                prefix: old.clone().into(),
                reader: ChunkSourceReader::default(),
                bytes: Vec::new(),
                offset: 0,
                counters,
                cancelled,
            };
            chunker.chunk_reader(input, &mut on_chunk)?
        };
        if chunk_index < old.len()
            || !noland_snapshot::same_file_identity(
                &captured_identity,
                &captured_source.metadata()?,
            )
            || !noland_snapshot::same_file_identity(&metadata, &source.metadata()?)
            || &Identity::read(job)? != identity
            || summary.size != identity.size
        {
            return Err(StateError::Conflict(
                "streaming source changed during capture; no backup was committed".into(),
            ));
        }
        pending.files.push((
            index,
            file_result(job.clone(), &metadata, summary, elapsed_ms(started)),
        ));
        if pending.files.len() >= 256 {
            flush(&mut pending)?;
            pending_bytes = 0;
        }
    }
    if !prefix.is_empty() {
        return Err(StateError::Integrity("unconsumed capture prefix".into()));
    }
    pending.done = true;
    flush(&mut pending)
}

fn file_result(
    job: HashJob,
    metadata: &std::fs::Metadata,
    summary: ChunkStreamResult,
    elapsed: u64,
) -> HashResult {
    let mut record = job.record;
    record.size = Some(summary.size.min(i64::MAX as u64) as i64);
    record.mtime_ns = metadata_mtime_ns(metadata);
    record.content_hash = Some(summary.file_hash.clone());
    #[cfg(unix)]
    {
        record.inode = Some(metadata.ino().min(i64::MAX as u64) as i64);
        record.mount_id = Some(metadata.dev().min(i64::MAX as u64) as i64);
        record.mode = Some(metadata.mode() as i64);
        record.uid = Some(metadata.uid() as i64);
        record.gid = Some(metadata.gid() as i64);
    }
    let manifest_file = ManifestFile {
        logical_root: job.logical.logical_root.as_token(),
        relative_path: job.logical.relative_path,
        source_path_hint: Some(record.canonical_path.clone()),
        file_type: "file".into(),
        size: summary.size,
        file_hash: summary.file_hash,
        chunks: summary.chunks,
        mode: record.mode.and_then(|v| v.try_into().ok()),
        mtime_ns: record.mtime_ns,
        uid: record.uid.and_then(|v| v.try_into().ok()),
        gid: record.gid.and_then(|v| v.try_into().ok()),
        symlink_target: None,
        persistence_class: job.association.persistence_class,
        semantic_role: job.association.semantic_role,
        association_confidence: job.association.confidence,
        shared_app_ids: job.shared_app_ids,
    };
    let mut file_state = file_state_from_manifest(&job.app_id, &manifest_file, &record);
    if job.staged.is_some() {
        file_state.trust = FileStateTrust::VerifyRequired;
    }
    HashResult {
        snapshot_duration_ms: elapsed,
        consistency: ConsistencyKind::Snapshot,
        file_state,
        metrics: HashMetricsDelta {
            bytes_hashed: manifest_file.size,
            ..Default::default()
        },
        manifest_file,
        new_chunks: Vec::new(),
        remote_entries: Vec::new(),
        cas_observations: Vec::new(),
        small_file_chunk_hashes: Vec::new(),
    }
}

struct PackWorkers {
    sender: Option<tokio::sync::mpsc::Sender<Plan>>,
    receiver: tokio::sync::mpsc::Receiver<Result<(usize, BuiltPack, u64)>>,
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}
impl Drop for PackWorkers {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.sender.take();
        self.receiver.close();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn pack_workers(
    agent: &StateAgent,
    master: &MasterKey,
    operation: Uuid,
    directory: &Path,
    output: &Path,
    count: usize,
    capacity: usize,
    counters: Arc<Counters>,
    stop: Arc<AtomicBool>,
) -> PackWorkers {
    let (sender, receiver) = tokio::sync::mpsc::channel::<Plan>(capacity);
    let receiver = Arc::new(Mutex::new(receiver));
    let (results, result_rx) = tokio::sync::mpsc::channel(capacity);
    let mut pool = PackWorkers {
        sender: Some(sender),
        receiver: result_rx,
        stop: stop.clone(),
        threads: Vec::new(),
    };
    for _ in 0..count {
        let (receiver, results, stop, counters) = (
            receiver.clone(),
            results.clone(),
            stop.clone(),
            counters.clone(),
        );
        let (directory, output, master) = (directory.to_owned(), output.to_owned(), master.clone());
        let cas_root = agent.config.paths.cache.join("cas/chunks");
        let operations = agent.operations.clone();
        pool.threads.push(std::thread::spawn(move || {
            loop {
                let Some(plan) = receiver
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .blocking_recv()
                else {
                    break;
                };
                if stop.load(Ordering::Acquire) || operations.cancel_requested(operation) {
                    break;
                }
                counters.active_codecs.fetch_add(1, Ordering::Relaxed);
                let started = Instant::now();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let cas = LocalCas::new(&cas_root)?;
                    let mut checked = HashMap::new();
                    for chunk in &plan.chunks {
                        packing::validate_lease(&cas, &chunk.lease)?;
                        packing::validate_source(
                            &cas,
                            &chunk.lease,
                            &chunk.hash,
                            &chunk.source,
                            &mut checked,
                        )?;
                    }
                    let mut packs = pack_chunk_files_with_limits(
                        &output,
                        &master,
                        plan.chunks
                            .iter()
                            .map(|c| (c.hash.clone(), c.source.clone())),
                        |_| stop.load(Ordering::Acquire) || operations.cancel_requested(operation),
                        constants::PACK_MAX,
                        constants::PACK_MAX,
                    )?;
                    // Cancellation in the pack API skips remaining inputs. Such
                    // a partial pack must never become this plan's durable receipt.
                    if stop.load(Ordering::Acquire) || operations.cancel_requested(operation) {
                        for pack in packs {
                            let _ = std::fs::remove_file(pack.path);
                        }
                        return Err(cancellation_error());
                    }
                    if packs.len() != 1 {
                        return Err(StateError::Integrity(
                            "streaming plan produced multiple packs".into(),
                        ));
                    }
                    let pack = packs.pop().unwrap();
                    if pack.entries.len() != plan.chunks.len()
                        || pack
                            .entries
                            .iter()
                            .zip(&plan.chunks)
                            .any(|(entry, chunk)| entry.chunk_hash != chunk.hash)
                    {
                        return Err(StateError::Integrity(
                            "sealed stream pack differs from plan".into(),
                        ));
                    }
                    packing::write_receipt(
                        &directory.join("sealed").join(format!("{}.enc", plan.index)),
                        &master,
                        &format!("backup-stream:{operation}:sealed:{}", plan.index),
                        &pack,
                    )?;
                    Ok((plan.index, pack, elapsed_ms(started)))
                }))
                .unwrap_or_else(|_| Err(StateError::Storage("pack worker panicked".into())));
                counters.active_codecs.fetch_sub(1, Ordering::Relaxed);
                let failed = result.is_err();
                if results.blocking_send(result).is_err() || failed {
                    break;
                }
            }
        }));
    }
    pool
}

fn load_plans(directory: &Path, master: &MasterKey, operation: Uuid) -> Result<VecDeque<Plan>> {
    let directory = directory.join("plans");
    let mut indices = Vec::new();
    if directory.exists() {
        for entry in std::fs::read_dir(&directory)? {
            let path = entry?.path();
            if path.extension().is_some_and(|v| v == "enc") {
                indices.push(
                    path.file_stem()
                        .and_then(|v| v.to_str())
                        .and_then(|v| v.parse::<usize>().ok())
                        .ok_or_else(|| {
                            StateError::Integrity("invalid streaming plan name".into())
                        })?,
                );
            }
        }
    }
    indices.sort_unstable();
    let mut plans = VecDeque::new();
    for (expected, index) in indices.into_iter().enumerate() {
        if expected != index {
            return Err(StateError::Integrity("streaming plan sequence gap".into()));
        }
        let plan: Plan = read(
            &directory.join(format!("{index}.enc")),
            master,
            &format!("backup-stream:{operation}:plan:{index}"),
        )?
        .ok_or_else(|| StateError::Integrity("missing streaming plan".into()))?;
        if plan.index != index || plan.chunks.is_empty() || plan.bytes()? > constants::PACK_MAX {
            return Err(StateError::Integrity("invalid streaming plan".into()));
        }
        plans.push_back(plan);
    }
    Ok(plans)
}

fn validate_resume_capture(
    directory: &Path,
    state: &State,
    master: &MasterKey,
    operation: Uuid,
) -> Result<()> {
    for work in 0..state.ranges.len() {
        let path = directory.join("capture").join(work.to_string());
        if !path.exists() {
            continue;
        }
        // A published capture directory always has a durable lease identity.
        // Never create a replacement lease when retry metadata was lost or
        // corrupted: existing plans may still reference its immutable inputs.
        read::<PathBuf>(
            &path.join("lease.enc"),
            master,
            &format!("backup-stream:{operation}:capture:{work}:lease"),
        )?
        .ok_or_else(|| StateError::Integrity("missing streaming capture lease receipt".into()))?;
        let mut indices = Vec::new();
        for entry in std::fs::read_dir(&path)? {
            let name = entry?.file_name();
            let name = name.to_string_lossy();
            if name == "lease.enc" || !name.ends_with(".enc") {
                continue;
            }
            indices.push(
                name.trim_end_matches(".enc")
                    .parse::<usize>()
                    .map_err(|_| {
                        StateError::Integrity("invalid streaming capture receipt name".into())
                    })?,
            );
        }
        indices.sort_unstable();
        let mut done = false;
        for (expected, index) in indices.into_iter().enumerate() {
            if index != expected || done {
                return Err(StateError::Integrity(
                    "streaming capture receipt sequence gap".into(),
                ));
            }
            let receipt: Receipt = read(
                &path.join(format!("{index}.enc")),
                master,
                &format!("backup-stream:{operation}:capture:{work}:{index}"),
            )?
            .ok_or_else(|| StateError::Integrity("missing streaming capture receipt".into()))?;
            done = receipt.done;
        }
    }
    Ok(())
}

pub(super) async fn run(
    agent: &StateAgent,
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    operation: Uuid,
    prepared: &mut PreparedBackup,
    mut state: State,
) -> Result<()> {
    // A restart can have less memory or a newly active game. Frozen input/pack
    // identities survive retry; the saved worker count is only an upper bound.
    let available = preparation::Budget::detect(agent, BackupPerformanceMode::Automatic)?;
    state.budget.hashing = state.budget.hashing.min(available.hashing);
    state.budget.packing = state.budget.packing.min(available.packing);
    let directory = prepared_backup_path(agent, operation).with_extension("stream");
    if let Some(mut record) = agent.db.get_operation(operation)? {
        persist_operation(
            agent,
            &mut record,
            BackupState::CapturingAndUploading,
            &prepared.metrics,
        )?;
    }
    let output = agent
        .config
        .paths
        .packs
        .join(prepared.manifest.bundle_id.to_string());
    let tuning = provider.transfer_tuning().normalized();
    let slots = tuning
        .max_parallel_uploads
        .min(tuning.max_bulk_files)
        .clamp(1, 8);
    // This budget covers compression + ready + active uploads, independently of
    // CPU worker count. Capture queues have their own bounded segment admission.
    let capacity = slots.saturating_mul(2);
    let byte_limit = tuning.max_bulk_bytes;
    let pack_max = constants::PACK_MAX.min(byte_limit);
    let requested_target = std::env::var("NOLAND_STREAM_PACK_MIB")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| matches!(v, 128 | 256 | 512))
        .map(|v| v * 1024 * 1024)
        .unwrap_or(PACK_BYTES);
    let pack_target = requested_target
        .min(pack_max)
        .min((byte_limit / slots as u64).max(1));
    let session =
        noland_storage::PackUploadSession::new(provider, Some(&agent.db), Some(operation));
    let mut uploads = noland_storage::RollingUploadWindow::new(slots, byte_limit);
    let mut plans = load_plans(&directory, master, operation)?;
    // Recovery validates persisted metadata/ciphertext before remote effects.
    // New capture still streams immediately; this only walks existing receipts.
    validate_resume_capture(&directory, &state, master, operation)?;
    let mut sealed = HashMap::new();
    for plan in &plans {
        if let Some(pack) = packing::read_receipt(
            &directory.join("sealed").join(format!("{}.enc", plan.index)),
            master,
            &format!("backup-stream:{operation}:sealed:{}", plan.index),
        )? {
            packing::validate_receipt(agent, operation, &output, &plan.packing(), &pack)?;
            sealed.insert(plan.index, pack);
        }
    }
    let mut next_plan = plans.len();
    let mut seen = prepared
        .pack_index
        .iter()
        .map(|p| p.chunk_hash.clone())
        .collect::<HashSet<_>>();
    for plan in &plans {
        seen.extend(plan.chunks.iter().map(|c| c.hash.clone()));
    }
    let binding = match repository_cache_binding(provider.storage_identity(), master) {
        Some(binding)
            if agent
                .db
                .has_remote_content(&binding, ContentObjectKind::Chunk)? =>
        {
            Some(binding)
        }
        _ => None,
    };
    let remote = RemoteChunkIndex {
        entries: HashMap::new(),
        scoped: binding.map(|v| {
            scoped_index::ScopedIndex::new(agent.db.clone(), v, master.clone(), Utc::now())
        }),
    };
    let state = Arc::new(state);
    let counters = Arc::new(Counters::default());
    let stop = Arc::new(AtomicBool::new(false));
    let mut capture = capture_workers(
        agent,
        operation,
        master,
        state.clone(),
        &directory,
        counters.clone(),
        stop.clone(),
    );
    let mut codecs = pack_workers(
        agent,
        master,
        operation,
        &directory,
        &output,
        state.budget.packing,
        capacity,
        counters.clone(),
        stop,
    );
    let mut capture_closed = false;
    let mut pending_chunks = VecDeque::<CapturedChunk>::new();
    let mut builders: [Vec<CapturedChunk>; 2] = [Vec::new(), Vec::new()];
    let mut builder_bytes = [24u64; 2];
    let mut builder_started: [Option<Instant>; 2] = [None, None];
    let mut ready = VecDeque::new();
    let mut reserved = HashMap::<usize, u64>::new();
    let mut reserved_bytes = 0u64;
    let mut codec_count = 0usize;
    let mut leases = HashSet::new();
    let mut completed_files = HashSet::new();
    let mut prepared_bytes = 0u64;
    let started = Instant::now();
    let mut first_upload_ms = None;
    let mut upload_started = None;
    let mut upload_active_ms = 0u64;
    let base_files = prepared.manifest.files.len() as u64;
    let base_bytes = prepared.manifest.files.iter().map(|f| f.size).sum::<u64>();
    let source_bytes = state.jobs.iter().map(|(_, i)| i.size).sum::<u64>();
    let mut peak_uploads = 0;
    let mut peak_reserved_bytes = 0;
    let mut last_loop = Instant::now();
    let mut slot_starvation_ms = 0u64;
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        if uploads.len() < slots && ready.is_empty() && !capture_closed {
            slot_starvation_ms += elapsed_ms(last_loop);
        }
        last_loop = Instant::now();
        let mut advanced = false;
        if agent.operations.cancel_requested(operation) {
            return Err(cancellation_error());
        }
        if plans.len() < capacity {
            while let Some(chunk) = pending_chunks.pop_front() {
                if seen.contains(&chunk.hash) {
                    prepared.metrics.num_chunks_reused += 1;
                    continue;
                }
                if let Some(entry) = remote.lookup(&chunk.hash)? {
                    seen.insert(chunk.hash.clone());
                    prepared.metrics.num_remote_index_hits += 1;
                    prepared.metrics.num_chunks_reused += 1;
                    prepared.pack_index.push(entry);
                    continue;
                }
                let kind = usize::from(chunk.small_save);
                let target = if kind == 1 {
                    (16 * 1024 * 1024).min(pack_target)
                } else if next_plan == 0 {
                    FIRST_PACK_BYTES.min(pack_target)
                } else {
                    pack_target
                };
                let record_bytes = chunk.source.len()? + 52;
                if 24 + record_bytes > pack_max {
                    return Err(StateError::Invalid(
                        "streaming chunk exceeds transfer byte budget".into(),
                    ));
                }
                if !builders[kind].is_empty()
                    && builder_bytes[kind] + record_bytes > target.min(pack_max)
                {
                    pending_chunks.push_front(chunk);
                    let plan = Plan {
                        index: next_plan,
                        chunks: std::mem::take(&mut builders[kind]),
                    };
                    write_batch(
                        &directory.join("plans").join(format!("{next_plan}.enc")),
                        &format!("backup-stream:{operation}:plan:{next_plan}"),
                        master,
                        &plan,
                    )?;
                    next_plan += 1;
                    builder_bytes[kind] = 24;
                    builder_started[kind] = None;
                    plans.push_back(plan);
                    advanced = true;
                    break;
                }
                seen.insert(chunk.hash.clone());
                prepared.metrics.num_chunks_created += 1;
                builder_bytes[kind] += record_bytes;
                builder_started[kind].get_or_insert_with(Instant::now);
                builders[kind].push(chunk);
                if builder_bytes[kind] >= target {
                    let plan = Plan {
                        index: next_plan,
                        chunks: std::mem::take(&mut builders[kind]),
                    };
                    write_batch(
                        &directory.join("plans").join(format!("{next_plan}.enc")),
                        &format!("backup-stream:{operation}:plan:{next_plan}"),
                        master,
                        &plan,
                    )?;
                    next_plan += 1;
                    builder_bytes[kind] = 24;
                    builder_started[kind] = None;
                    plans.push_back(plan);
                    advanced = true;
                    break;
                }
            }
            if pending_chunks.is_empty() {
                for kind in 0..2 {
                    if !builders[kind].is_empty()
                        && plans.len() < capacity
                        && (capture_closed
                            || builder_started[kind]
                                .is_some_and(|start| start.elapsed() >= Duration::from_secs(1)))
                    {
                        let plan = Plan {
                            index: next_plan,
                            chunks: std::mem::take(&mut builders[kind]),
                        };
                        write_batch(
                            &directory.join("plans").join(format!("{next_plan}.enc")),
                            &format!("backup-stream:{operation}:plan:{next_plan}"),
                            master,
                            &plan,
                        )?;
                        next_plan += 1;
                        builder_bytes[kind] = 24;
                        builder_started[kind] = None;
                        plans.push_back(plan);
                        advanced = true;
                    }
                }
            }
        }
        while let Some(plan) = plans.front() {
            let bytes = plan.bytes()?;
            if bytes > byte_limit {
                return Err(StateError::Invalid(
                    "retained streaming plan exceeds transfer byte budget".into(),
                ));
            }
            if reserved.len() < capacity && reserved_bytes.saturating_add(bytes) <= byte_limit {
                let plan = plans.pop_front().unwrap();
                reserved.insert(plan.index, bytes);
                reserved_bytes += bytes;
                advanced = true;
                peak_reserved_bytes = peak_reserved_bytes.max(reserved_bytes);
                if let Some(pack) = sealed.remove(&plan.index) {
                    if pack.bytes > bytes {
                        return Err(StateError::Integrity(
                            "retained pack exceeds its reservation".into(),
                        ));
                    }
                    reserved_bytes -= bytes - pack.bytes;
                    reserved.insert(plan.index, pack.bytes);
                    prepared_bytes += pack.bytes;
                    ready.push_back((plan.index, pack));
                } else {
                    codecs
                        .sender
                        .as_ref()
                        .unwrap()
                        .send(plan)
                        .await
                        .map_err(|_| StateError::Storage("pack workers stopped".into()))?;
                    codec_count += 1;
                }
            } else {
                break;
            }
        }
        while ready
            .front()
            .is_some_and(|(_, pack): &(usize, BuiltPack)| uploads.can_push(pack.bytes))
        {
            let (index, pack) = ready.pop_front().unwrap();
            first_upload_ms.get_or_insert(elapsed_ms(started));
            upload_started.get_or_insert_with(Instant::now);
            let session = &session;
            uploads.push_tracked(1, pack.bytes, |reservation| async move {
                session
                    .upload_reserved(&[(pack.pack_id.clone(), pack.path.clone())], reservation)
                    .await?;
                Ok((index, pack))
            })?;
            peak_uploads = peak_uploads.max(uploads.len());
            advanced = true;
        }
        let finished = capture_closed
            && pending_chunks.is_empty()
            && builders.iter().all(Vec::is_empty)
            && plans.is_empty()
            && codec_count == 0
            && ready.is_empty()
            && uploads.is_empty();
        if finished {
            break;
        }
        // Drain existing metadata/builders before receiving another segment.
        // Yield to uploads and compressors on every iteration, even if capture
        // is continuously ready, so neither producers nor a straggler monopolize.
        tokio::select! {
            receipt = capture.receiver.recv(), if !capture_closed && pending_chunks.is_empty() && plans.len() < capacity => {
                match receipt {
                    None => capture_closed = true,
                    Some(result) => {
                        let receipt = result?;
                        for chunk in &receipt.chunks { leases.insert(chunk.lease.clone()); }
                        pending_chunks.extend(receipt.chunks);
                        for (index, result) in receipt.files {
                            if !completed_files.insert(index) { return Err(StateError::Integrity("duplicate complete file".into())); }
                            apply_result(prepared, result);
                        }
                    }
                }
            }
            result = codecs.receiver.recv(), if codec_count > 0 => {
                let (index, pack, milliseconds) = result.ok_or_else(|| StateError::Storage("pack workers stopped".into()))??;
                codec_count -= 1;
                let reservation = reserved.get_mut(&index).ok_or_else(|| StateError::Integrity("unexpected sealed pack".into()))?;
                if pack.bytes > *reservation { return Err(StateError::Integrity("sealed pack exceeds its reservation".into())); }
                reserved_bytes -= *reservation - pack.bytes; *reservation = pack.bytes;
                prepared.metrics.packing_duration_ms += milliseconds;
                prepared.metrics.bytes_packed += pack.bytes;
                prepared_bytes += pack.bytes;
                ready.push_back((index, pack));
            }
            result = uploads.next(), if !uploads.is_empty() => {
                let (index, pack) = result.unwrap()?;
                reserved_bytes = reserved_bytes.saturating_sub(reserved.remove(&index).unwrap());
                prepared.incremental += pack.bytes;
                prepared.checksums.insert(pack.pack_id.clone(), pack.checksum);
                prepared.pack_files.push((pack.pack_id, pack.path));
                prepared.pack_index.extend(pack.entries.iter().cloned());
                prepared.new_pack_entries.extend(pack.entries);
                if uploads.is_empty() { upload_active_ms += upload_started.take().map(elapsed_ms).unwrap_or(0); }
            }
            _ = tick.tick() => {
                let mut progress = agent.db.get_operation_progress(operation)?.unwrap_or_else(|| OperationProgress::new("capture_and_upload", 0));
                progress.phase = "capture_and_upload".into();
                progress.unit = Some("files".into()); progress.total_units = Some(base_files + state.jobs.len() as u64);
                progress.completed_units = base_files + completed_files.len() as u64;
                progress.message = Some("Capturing, packing and uploading application files".into());
                progress.detail_json["streaming_pipeline"] = serde_json::json!({
                    "version":3, "selected_files":base_files + state.jobs.len() as u64, "source_total_bytes":base_bytes + source_bytes,
                    "reused_files":base_files, "reused_bytes":base_bytes, "capture_total_bytes":source_bytes,
                    "read_bytes":counters.read_bytes.load(Ordering::Relaxed), "durable_bytes":counters.durable_bytes.load(Ordering::Relaxed),
                    "completed_files":completed_files.len(), "prepared_bytes":prepared_bytes, "first_upload_ms":first_upload_ms,
                    "active_readers":counters.active_readers.load(Ordering::Relaxed), "reader_limit":state.budget.hashing.min(4),
                    "active_codecs":counters.active_codecs.load(Ordering::Relaxed), "codec_limit":state.budget.packing,
                    "ready_packs":ready.len(), "inflight_uploads":uploads.len(), "upload_limit":slots,
                    "reserved_bytes":reserved_bytes, "reserved_byte_limit":byte_limit, "capture_wait_us":counters.capture_wait_us.load(Ordering::Relaxed),
                    "capture_queue_limit":state.budget.hashing.min(4) * 2, "capture_segment_bytes":SEGMENT_BYTES,
                    "codec_buffer_limit_bytes":state.budget.packing as u64 * 32 * 1024 * 1024,
                    "retained_capture_logical_bytes":counters.durable_bytes.load(Ordering::Relaxed), "retained_ciphertext_bytes":prepared_bytes,
                    "ready_ciphertext_bytes":ready.iter().map(|(_,p)| p.bytes).sum::<u64>(), "active_upload_bytes":uploads.bytes(),
                    "upload_slot_starvation_ms":slot_starvation_ms,
                    "peak_uploads":peak_uploads, "peak_reserved_bytes":peak_reserved_bytes, "capture_complete":capture_closed,
                    "elapsed_ms":elapsed_ms(started), "waiting_on":if !ready.is_empty() { "upload_capacity" } else if codec_count > 0 { "compression" } else if !capture_closed { "capture" } else { "provider_completion" }
                });
                progress.updated_at = Utc::now(); agent.db.set_operation_progress(operation, Some(&progress))?;
                agent.db.set_operation_metrics(operation, &prepared.metrics)?;
            }
            _ = tokio::task::yield_now(), if advanced || (!pending_chunks.is_empty() && plans.len() < capacity) => {}
        }
    }
    // Join every producer before retiring any capture inputs.
    drop(codecs);
    drop(capture);
    drop(uploads);
    if completed_files.len() != state.jobs.len() {
        return Err(StateError::Integrity(
            "streaming capture lost file receipts".into(),
        ));
    }
    for work in 0..state.ranges.len() {
        if let Some(lease) = read::<PathBuf>(
            &directory
                .join("capture")
                .join(work.to_string())
                .join("lease.enc"),
            master,
            &format!("backup-stream:{operation}:capture:{work}:lease"),
        )? {
            leases.insert(lease);
        }
    }
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
        .collect::<HashSet<_>>();
    prepared
        .pack_index
        .retain(|entry| referenced.contains(&entry.chunk_hash));
    let indexed = prepared
        .pack_index
        .iter()
        .map(|entry| &entry.chunk_hash)
        .collect::<HashSet<_>>();
    if referenced != indexed {
        return Err(StateError::Integrity(
            "streaming manifest has missing pack references".into(),
        ));
    }
    let lengths = prepared
        .pack_index
        .iter()
        .map(|entry| (&entry.chunk_hash, u64::from(entry.plaintext_len)))
        .collect::<HashMap<_, _>>();
    if prepared
        .manifest
        .files
        .iter()
        .flat_map(|file| &file.chunks)
        .any(|chunk| lengths.get(&chunk.hash) != Some(&chunk.size))
    {
        return Err(StateError::Integrity(
            "streaming manifest chunk length mismatch".into(),
        ));
    }
    if prepared.manifest.mode == BackupMode::CompleteApplication
        && prepared.manifest.files.is_empty()
    {
        return Err(StateError::Invalid(
            "complete backup contains no application files".into(),
        ));
    }
    let mode = restore_mode_for_backup(prepared.manifest.mode);
    noland_restore::embed_restore_plan(&mut prepared.manifest, mode);
    prepared.metrics.hashing_duration_ms = elapsed_ms(started);
    prepared.metrics.upload_duration_ms += upload_active_ms;
    // Publish a complete snapshot even when a tiny capture finishes before the
    // first progress tick. Clear live reservations without erasing disk-retained
    // ciphertext or leaving a stale worker/queue count during manifest commit.
    let mut progress = agent
        .db
        .get_operation_progress(operation)?
        .unwrap_or_else(|| OperationProgress::new("capture_and_upload", 0));
    progress.phase = "committing".into();
    progress.unit = Some("files".into());
    progress.total_units = Some(base_files + state.jobs.len() as u64);
    progress.completed_units = base_files + completed_files.len() as u64;
    progress.message = Some("Encrypted packs uploaded; committing backup metadata".into());
    progress.detail_json["streaming_pipeline"] = serde_json::json!({
        "version":3, "selected_files":base_files + state.jobs.len() as u64, "source_total_bytes":base_bytes + source_bytes,
        "reused_files":base_files, "reused_bytes":base_bytes, "capture_total_bytes":source_bytes,
        "read_bytes":counters.read_bytes.load(Ordering::Relaxed), "durable_bytes":counters.durable_bytes.load(Ordering::Relaxed),
        "completed_files":completed_files.len(), "prepared_bytes":prepared_bytes, "first_upload_ms":first_upload_ms,
        "active_readers":0, "reader_limit":state.budget.hashing.min(4), "active_codecs":0, "codec_limit":state.budget.packing,
        "ready_packs":0, "inflight_uploads":0, "upload_limit":slots, "reserved_bytes":0, "reserved_byte_limit":byte_limit,
        "capture_wait_us":counters.capture_wait_us.load(Ordering::Relaxed), "capture_queue_limit":state.budget.hashing.min(4) * 2,
        "capture_segment_bytes":SEGMENT_BYTES, "codec_buffer_limit_bytes":state.budget.packing as u64 * 32 * 1024 * 1024,
        "retained_capture_logical_bytes":0, "retained_ciphertext_bytes":prepared_bytes, "ready_ciphertext_bytes":0, "active_upload_bytes":0,
        "upload_slot_starvation_ms":slot_starvation_ms, "peak_uploads":peak_uploads, "peak_reserved_bytes":peak_reserved_bytes,
        "capture_complete":true, "elapsed_ms":elapsed_ms(started), "waiting_on":"manifest_commit"
    });
    progress.updated_at = Utc::now();
    prepared.release_capture = Some(Cleanup {
        leases: leases.into_iter().collect(),
        snapshot: state.snapshot.clone(),
    });
    write_prepared_backup(agent, master, operation, prepared)?;
    cleanup(agent, operation, prepared)?;
    agent.db.set_operation_progress(operation, Some(&progress))
}
