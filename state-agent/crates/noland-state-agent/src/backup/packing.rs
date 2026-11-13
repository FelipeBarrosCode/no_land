//! Bounded pack sealing / upload overlap with authenticated per-pack receipts.
//! The captured revision is persisted once, before any network side effects.
use super::*;
use noland_cas::ChunkSource;
use noland_storage::{PackUploadSession, RollingUploadWindow};
use std::collections::VecDeque;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Condvar, Mutex,
};

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct PackingState {
    pub plans: Vec<PackPlan>,
    pub lease: PathBuf,
    pub workers: usize,
    #[serde(default)]
    pub receipt_prefix: String,
    #[serde(default)]
    pub partial_capture: bool,
    #[serde(default)]
    pub upload_slots: Option<usize>,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct PackPlan {
    pub(super) chunks: Vec<(String, ChunkSource)>,
    pub(super) target: u64,
    pub(super) max: u64,
}

pub(super) fn plans(
    small: Vec<(String, ChunkSource)>,
    regular: Vec<(String, ChunkSource)>,
) -> Result<Vec<PackPlan>> {
    let mut plans = Vec::new();
    for (chunks, target, max) in [
        (small, 16 * 1024 * 1024, 32 * 1024 * 1024),
        (regular, constants::PACK_TARGET, constants::PACK_MAX),
    ] {
        plans.extend(
            group_sources(chunks, target, max)?
                .into_iter()
                .map(|chunks| PackPlan {
                    chunks,
                    target,
                    max,
                }),
        );
    }
    Ok(plans)
}

fn group_sources(
    chunks: Vec<(String, ChunkSource)>,
    target: u64,
    max: u64,
) -> Result<Vec<Vec<(String, ChunkSource)>>> {
    if target == 0 || target > max {
        return Err(StateError::Invalid("invalid pack size limits".into()));
    }
    let mut groups = Vec::new();
    let mut group = Vec::new();
    let mut bytes = 24u64;
    for (hash, source) in chunks {
        let record = source
            .len()?
            .checked_add(52)
            .ok_or_else(|| StateError::Invalid("chunk length exceeds pack format".into()))?;
        if record.checked_add(24).is_none_or(|size| size > max) {
            return Err(StateError::Invalid(
                "chunk exceeds pack hard maximum".into(),
            ));
        }
        if !group.is_empty()
            && (bytes >= target || bytes.checked_add(record).is_none_or(|size| size > max))
        {
            groups.push(std::mem::take(&mut group));
            bytes = 24;
        }
        bytes += record;
        group.push((hash, source));
    }
    if !group.is_empty() {
        groups.push(group);
    }
    Ok(groups)
}

pub(super) fn validate_lease(cas: &LocalCas, lease: &Path) -> Result<()> {
    if lease.parent() != Some(cas.root().join(".leases").as_path())
        || lease.file_name().is_none()
        || lease
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(StateError::UnsafePath("capture lease outside CAS".into()));
    }
    for path in lease
        .ancestors()
        .filter(|path| !path.as_os_str().is_empty())
    {
        let meta = std::fs::symlink_metadata(path)?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err(StateError::UnsafePath(
                "invalid capture lease directory".into(),
            ));
        }
    }
    Ok(())
}

pub(super) fn validate_source(
    cas: &LocalCas,
    lease: &Path,
    hash: &str,
    source: &ChunkSource,
    checked: &mut HashMap<PathBuf, u64>,
) -> Result<u64> {
    let canonical = cas.path_for_hash(hash)?; // Validate the hash even for segment slices.
    let path = source.path();
    match source {
        ChunkSource::File(_) => {
            if path != lease.join(canonical.file_name().expect("chunk filename")) {
                return Err(StateError::UnsafePath("invalid capture chunk path".into()));
            }
        }
        ChunkSource::Slice { .. } => {
            let segments = lease.join(".segments");
            if path.parent() != Some(segments.as_path())
                || path.extension().and_then(|value| value.to_str()) != Some("capture")
            {
                return Err(StateError::UnsafePath(
                    "invalid capture segment path".into(),
                ));
            }
            if let std::collections::hash_map::Entry::Vacant(entry) = checked.entry(segments) {
                let meta = std::fs::symlink_metadata(entry.key())?;
                if !meta.is_dir() || meta.file_type().is_symlink() {
                    return Err(StateError::UnsafePath(
                        "invalid capture segment directory".into(),
                    ));
                }
                entry.insert(0);
            }
        }
    }
    let file_bytes = match checked.get(path) {
        Some(bytes) => *bytes,
        None => {
            let meta = std::fs::symlink_metadata(path)?;
            if !meta.is_file() || meta.file_type().is_symlink() {
                return Err(StateError::UnsafePath(
                    "capture source is not a regular file".into(),
                ));
            }
            checked.insert(path.to_path_buf(), meta.len());
            meta.len()
        }
    };
    if let ChunkSource::Slice { offset, len, .. } = source {
        if offset.checked_add(*len).is_none_or(|end| end > file_bytes) {
            return Err(StateError::Integrity(
                "capture slice exceeds its sealed segment".into(),
            ));
        }
    }
    match source {
        ChunkSource::File(_) => Ok(file_bytes),
        ChunkSource::Slice { .. } => source.len(),
    }
}

#[derive(Default)]
struct ReadyUsage {
    count: usize,
    bytes: u64,
    cancelled: bool,
}

/// Reserves space before sealing, so a fast compressor cannot build an
/// unbounded ready queue behind a slow network. Retry ciphertext stays on disk;
/// these limits cover active sealing and queued (not yet uploading) packs.
struct ReadyBudget {
    max_count: usize,
    max_bytes: u64,
    usage: Mutex<ReadyUsage>,
    wake: Condvar,
}

impl ReadyBudget {
    fn new(max_count: usize, max_bytes: u64) -> Arc<Self> {
        Arc::new(Self {
            max_count,
            max_bytes,
            usage: Mutex::default(),
            wake: Condvar::new(),
        })
    }

    fn acquire(self: &Arc<Self>, bytes: u64) -> Result<ReadyPermit> {
        if bytes > self.max_bytes {
            return Err(StateError::Invalid("pack exceeds ready byte budget".into()));
        }
        let mut usage = self.usage.lock().unwrap_or_else(|error| error.into_inner());
        while !usage.cancelled
            && (usage.count >= self.max_count
                || usage
                    .bytes
                    .checked_add(bytes)
                    .is_none_or(|sum| sum > self.max_bytes))
        {
            usage = self
                .wake
                .wait(usage)
                .unwrap_or_else(|error| error.into_inner());
        }
        if usage.cancelled {
            return Err(cancellation_error());
        }
        usage.count += 1;
        usage.bytes += bytes;
        Ok(ReadyPermit {
            budget: Arc::clone(self),
            bytes,
        })
    }

    fn cancel(&self) {
        self.usage
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .cancelled = true;
        self.wake.notify_all();
    }

    fn snapshot(&self) -> (usize, u64) {
        let usage = self.usage.lock().unwrap_or_else(|error| error.into_inner());
        (usage.count, usage.bytes)
    }
}

struct ReadyPermit {
    budget: Arc<ReadyBudget>,
    bytes: u64,
}

impl ReadyPermit {
    fn shrink(&mut self, bytes: u64) {
        let bytes = bytes.min(self.bytes);
        self.budget
            .usage
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .bytes -= self.bytes - bytes;
        self.bytes = bytes;
        self.budget.wake.notify_all();
    }
}

impl Drop for ReadyPermit {
    fn drop(&mut self) {
        let mut usage = self
            .budget
            .usage
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        usage.count -= 1;
        usage.bytes -= self.bytes;
        self.budget.wake.notify_all();
    }
}

struct ReadyPack {
    pack: BuiltPack,
    _permit: Option<ReadyPermit>,
}

struct ActiveWorker<'a>(&'a AtomicUsize);
impl Drop for ActiveWorker<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Release);
    }
}

struct Workers {
    receiver: tokio::sync::mpsc::Receiver<Result<ReadyPack>>,
    cancelled: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
    budget: Arc<ReadyBudget>,
    active: Arc<AtomicUsize>,
}

impl Drop for Workers {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        // Wake blocked senders before joining. Cancellation never leaves a pack
        // writer running against a lease that its caller might release.
        self.receiver.close();
        self.budget.cancel();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn receipt_path(agent: &StateAgent, operation: Uuid, index: usize) -> PathBuf {
    prepared_backup_path(agent, operation)
        .with_extension("packs")
        .join(format!("{index}.enc"))
}

fn receipt_aad(operation: Uuid, index: usize) -> String {
    format!("backup-pack:{operation}:{index}")
}

pub(super) fn write_receipt(
    path: &Path,
    master: &MasterKey,
    aad: &str,
    pack: &BuiltPack,
) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().expect("receipt parent");
    std::fs::create_dir_all(parent)?;
    let encrypted = noland_storage::seal_metadata(
        &noland_crypto::derive_keys(master).manifest,
        aad.as_bytes(),
        pack,
    )?
    .envelope;
    let partial = path.with_extension("partial");
    disk::ensure_space(parent, encrypted.len() as u64)?;
    let mut file = std::fs::File::create(&partial)?;
    file.write_all(&encrypted)?;
    file.sync_all()?;
    std::fs::rename(partial, path)?;
    std::fs::File::open(parent)?.sync_all()?;
    if let Some(parent) = parent.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

pub(super) fn read_receipt(
    path: &Path,
    master: &MasterKey,
    aad: &str,
) -> Result<Option<BuiltPack>> {
    let encrypted = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok(Some(noland_storage::open_metadata(
        &noland_crypto::derive_keys(master).manifest,
        aad.as_bytes(),
        encrypted,
    )?))
}

pub(super) fn validate_receipt(
    agent: &StateAgent,
    operation: Uuid,
    directory: &Path,
    plan: &PackPlan,
    pack: &BuiltPack,
) -> Result<()> {
    let hashes = plan
        .chunks
        .iter()
        .map(|(hash, _)| hash.as_str())
        .collect::<BTreeSet<_>>();
    let stored_hashes = pack
        .entries
        .iter()
        .map(|entry| entry.chunk_hash.as_str())
        .collect::<BTreeSet<_>>();
    if Uuid::parse_str(&pack.pack_id).is_err()
        || pack.path != directory.join(format!("{}.pack", pack.pack_id))
        || pack.bytes > plan.max
        || hashes != stored_hashes
        || pack.entries.len() != hashes.len()
        || pack
            .entries
            .iter()
            .any(|entry| entry.pack_id != pack.pack_id)
    {
        return Err(StateError::Integrity(
            "pack receipt does not match its captured plan".into(),
        ));
    }
    if !agent
        .db
        .sync_journal_completed(operation, &pack_key(&pack.pack_id))?
        && (std::fs::symlink_metadata(&pack.path)?
            .file_type()
            .is_symlink()
            || std::fs::metadata(&pack.path)?.len() != pack.bytes
            || noland_cas::blake3_file(&pack.path)? != pack.checksum)
    {
        return Err(StateError::Integrity(
            "captured pack changed before retry".into(),
        ));
    }
    Ok(())
}

pub(super) async fn pack_and_upload(
    agent: &StateAgent,
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    operation: Uuid,
    prepared: &mut PreparedBackup,
    _resumed: bool,
) -> Result<()> {
    let session = PackUploadSession::new(provider, Some(&agent.db), Some(operation));
    pack_and_upload_in_session(agent, provider, master, operation, prepared, &session).await
}

pub(super) async fn pack_and_upload_in_session(
    agent: &StateAgent,
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    operation: Uuid,
    prepared: &mut PreparedBackup,
    session: &PackUploadSession<'_>,
) -> Result<()> {
    let Some(packing) = prepared.packing.take() else {
        return release_prepared_lease(agent, operation, prepared);
    };
    if !packing
        .receipt_prefix
        .bytes()
        .all(|b| b.is_ascii_digit() || b == b'-')
    {
        return Err(StateError::UnsafePath("invalid pack receipt scope".into()));
    }
    let receipt = |index| {
        let path = receipt_path(agent, operation, index);
        path.with_file_name(format!("{}{index}.enc", packing.receipt_prefix))
    };
    let aad = |index| {
        format!(
            "{}{}",
            receipt_aad(operation, index),
            packing.receipt_prefix
        )
    };
    let started = Instant::now();
    let cas = LocalCas::new(agent.config.paths.cache.join("cas/chunks"))?;
    validate_lease(&cas, &packing.lease)?;
    let tuning = provider.transfer_tuning().normalized();
    let upload_count = tuning
        .max_parallel_uploads
        .min(packing.upload_slots.unwrap_or(usize::MAX))
        .min(tuning.max_bulk_files)
        .clamp(1, 32);
    let upload_bytes = tuning.max_bulk_bytes;
    // Leave independent rolling requests available. Normal large packs retain
    // one request each; only already-ready small ciphertext can be grouped.
    // Partial capture has independent rolling batches above this window. It can
    // group the whole local share without monopolizing the operation's slots.
    let group_count = (if packing.partial_capture {
        upload_count
    } else {
        upload_count / 2
    })
    .clamp(1, 8);
    let group_bytes = 64 * 1024 * 1024;
    let directory = agent
        .config
        .paths
        .packs
        .join(prepared.manifest.bundle_id.to_string());
    let mut pending = VecDeque::new();
    let mut existing = VecDeque::new();
    let mut pending_bytes = 0u64;
    let mut checked_sources = HashMap::new();
    let total_plans = packing.plans.len();
    for (index, plan) in packing.plans.into_iter().enumerate() {
        if agent.operations.cancel_requested(operation) {
            return Err(cancellation_error());
        }
        if let Some(pack) = read_receipt(&receipt(index), master, &aad(index))? {
            validate_receipt(agent, operation, &directory, &plan, &pack)?;
            if pack.bytes > upload_bytes {
                return Err(StateError::Invalid(
                    "pack exceeds transfer byte budget".into(),
                ));
            }
            existing.push_back(pack);
        } else {
            let mut plan_bytes = 24u64;
            for (chunk_index, (hash, source)) in plan.chunks.iter().enumerate() {
                if chunk_index % 256 == 0 {
                    tokio::task::yield_now().await;
                    if agent.operations.cancel_requested(operation) {
                        return Err(cancellation_error());
                    }
                }
                let bytes =
                    validate_source(&cas, &packing.lease, hash, source, &mut checked_sources)?;
                // The sealing worker's read_verified authenticates these exact
                // bytes immediately before encryption. A retry prepass doubled
                // IO and allocation without closing any additional race.
                plan_bytes = plan_bytes.saturating_add(bytes).saturating_add(52);
            }
            if plan.target == 0
                || plan.target > plan.max
                || plan_bytes > plan.max
                || plan_bytes > upload_bytes
            {
                return Err(StateError::Invalid(
                    "captured plan exceeds pack/transfer limits".into(),
                ));
            }
            pending_bytes = pending_bytes.saturating_add(plan_bytes);
            pending.push_back((index, plan, receipt(index), plan_bytes));
        }
    }
    drop(checked_sources);
    // Estimate the complete output without holding the same bytes that each
    // bounded pack writer will reserve and consume as it allocates ciphertext.
    disk::check_admission(&[(directory.clone(), pending_bytes)])?;
    let count = packing.workers.clamp(1, 32).min(pending.len().max(1));
    // Network capacity, not CPU worker count, determines the upload/ready windows.
    let (sender, receiver) = tokio::sync::mpsc::channel(upload_count);
    let cancelled = Arc::new(AtomicBool::new(false));
    let pending = Arc::new(Mutex::new(pending));
    let mut workers = Workers {
        receiver,
        cancelled,
        threads: Vec::new(),
        budget: ReadyBudget::new(upload_count, upload_bytes),
        active: Arc::new(AtomicUsize::new(0)),
    };
    for _ in 0..count {
        let pending = Arc::clone(&pending);
        let stopped = Arc::clone(&workers.cancelled);
        let sender = sender.clone();
        let master = master.clone();
        let directory = directory.clone();
        let operations = agent.operations.clone();
        let budget = Arc::clone(&workers.budget);
        let active = Arc::clone(&workers.active);
        let receipt_prefix = packing.receipt_prefix.clone();
        workers.threads.push(std::thread::spawn(move || loop {
            if stopped.load(Ordering::Acquire) || operations.cancel_requested(operation) {
                break;
            }
            let Some((index, plan, receipt, bytes)) =
                pending.lock().expect("pack queue poisoned").pop_front()
            else {
                break;
            };
            let result = (|| {
                let mut permit = budget.acquire(bytes)?;
                if stopped.load(Ordering::Acquire) || operations.cancel_requested(operation) {
                    return Err(cancellation_error());
                }
                active.fetch_add(1, Ordering::Release);
                let _active = ActiveWorker(&active);
                let mut packs = pack_chunk_files_with_limits(
                    &directory,
                    &master,
                    plan.chunks,
                    |_| stopped.load(Ordering::Acquire) || operations.cancel_requested(operation),
                    plan.target,
                    plan.max,
                )?;
                if stopped.load(Ordering::Acquire) || operations.cancel_requested(operation) {
                    for pack in packs {
                        let _ = std::fs::remove_file(pack.path);
                    }
                    return Err(cancellation_error());
                }
                if packs.len() != 1 {
                    return Err(StateError::Integrity(
                        "captured plan must seal exactly one pack".into(),
                    ));
                }
                let pack = packs.pop().expect("one pack");
                permit.shrink(pack.bytes);
                write_receipt(
                    &receipt,
                    &master,
                    &format!("{}{}", receipt_aad(operation, index), receipt_prefix),
                    &pack,
                )?;
                Ok(ReadyPack {
                    pack,
                    _permit: Some(permit),
                })
            })();
            let failed = result.is_err();
            if failed {
                stopped.store(true, Ordering::Release);
                budget.cancel();
            }
            if sender.blocking_send(result).is_err() || failed {
                break;
            }
        }));
    }
    drop(sender);
    let mut progress = OperationProgress::new("packing_and_uploading", 0);
    progress.detail_json =
        serde_json::json!({"total_pack_plans": total_plans, "prepared_packs": 0});
    agent
        .db
        .set_operation_progress(operation, Some(&progress))?;
    let mut uploads = RollingUploadWindow::new(upload_count, upload_bytes);
    let mut ready: Option<ReadyPack> = None;
    let mut source_closed = false;
    let mut last_progress = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut upload_started: Option<Instant> = None;
    let mut upload_active_ms = 0u64;
    let mut peak_count = 0usize;
    let mut peak_bytes = 0u64;
    loop {
        if agent.operations.cancel_requested(operation) {
            return Err(cancellation_error());
        }
        if ready.is_none() {
            ready = existing.pop_front().map(|pack| ReadyPack {
                pack,
                _permit: None,
            });
        }
        if ready
            .as_ref()
            .is_some_and(|ready| uploads.can_push(ready.pack.bytes))
        {
            let mut packs = Vec::new();
            let mut bytes = 0u64;
            // Group only packs already ready. Never delay a first pack waiting
            // for compression or turn a group into an unaccounted transfer slot.
            while let Some(item) = ready.take() {
                if !uploads.can_push_count(packs.len() + 1, bytes.saturating_add(item.pack.bytes))
                    || (!packs.is_empty() && bytes.saturating_add(item.pack.bytes) > group_bytes)
                {
                    ready = Some(item);
                    break;
                }
                bytes += item.pack.bytes;
                packs.push(item.pack);
                drop(item._permit);
                if !provider.supports_parallel_upload_groups()
                    || packs.len() >= tuning.max_bulk_files.min(group_count)
                    || bytes >= group_bytes
                {
                    break;
                }
                ready = existing.pop_front().map(|pack| ReadyPack {
                    pack,
                    _permit: None,
                });
                if ready.is_none() && existing.is_empty() && !source_closed {
                    match workers.receiver.try_recv() {
                        Ok(item) => ready = Some(item?),
                        Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                            source_closed = true
                        }
                        Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {}
                    }
                }
            }
            if upload_started.is_none() {
                upload_started = Some(Instant::now());
            }
            uploads.push_tracked(packs.len(), bytes, |reservation| async move {
                let files = packs
                    .iter()
                    .map(|pack| (pack.pack_id.clone(), pack.path.clone()))
                    .collect::<Vec<_>>();
                session.upload_reserved(&files, reservation).await?;
                Ok(packs)
            })?;
            peak_count = peak_count.max(uploads.len());
            peak_bytes = peak_bytes.max(uploads.bytes());
        }
        let finished =
            source_closed && ready.is_none() && existing.is_empty() && uploads.is_empty();
        if finished || last_progress.elapsed() >= PROGRESS_FLUSH_INTERVAL {
            let (reserved_count, reserved_bytes) = workers.budget.snapshot();
            if let Some(mut progress) = agent.db.get_operation_progress(operation)? {
                progress.detail_json["prepared_packs"] =
                    serde_json::json!(prepared.pack_files.len());
                progress.detail_json["packing_upload_elapsed_ms"] =
                    serde_json::json!(elapsed_ms(started));
                progress.detail_json["pack_pipeline"] = serde_json::json!({
                    "phase": if finished { "transfers_complete" } else if source_closed && ready.is_none() && existing.is_empty() { "draining_uploads" } else if ready.is_some() { "upload_backpressure" } else if uploads.is_empty() { "waiting_for_pack" } else { "packing_and_uploading" },
                    "compression_workers": count,
                    "compression_workers_active": workers.active.load(Ordering::Acquire),
                    "pending_plans": pending.lock().expect("pack queue poisoned").len(),
                    "retry_packs_pending": existing.len() + usize::from(ready.as_ref().is_some_and(|item| item._permit.is_none())),
                    "ready_queue_count": workers.receiver.len() + usize::from(ready.as_ref().is_some_and(|item| item._permit.is_some())),
                    "sealing_and_ready_reserved_count": reserved_count,
                    "sealing_and_ready_reserved_bytes": reserved_bytes,
                    "ready_count_limit": upload_count,
                    "ready_byte_limit": upload_bytes,
                    "inflight_count": uploads.len(),
                    "inflight_bytes": uploads.bytes(),
                    "inflight_count_limit": upload_count,
                    "inflight_byte_limit": upload_bytes,
                    "peak_inflight_count": peak_count,
                    "peak_inflight_bytes": peak_bytes,
                    "upload_active_ms": upload_active_ms.saturating_add(upload_started.map(elapsed_ms).unwrap_or(0)),
                    "elapsed_ms": elapsed_ms(started),
                });
                progress.updated_at = Utc::now();
                agent
                    .db
                    .set_operation_progress(operation, Some(&progress))?;
            }
            last_progress = Instant::now();
        }
        if finished {
            break;
        }
        if ready.is_none() && !existing.is_empty() && uploads.len() < upload_count {
            continue;
        }
        // Refill without waiting for a straggler. Poll transfers even while the
        // next compressor is busy, and keep cancellation responsive during IO.
        tokio::select! {
            completed = uploads.next(), if !uploads.is_empty() => {
                let packs = completed.expect("active upload")?;
                for pack in packs {
                prepared.incremental = prepared.incremental.saturating_add(pack.bytes);
                prepared.metrics.bytes_packed = prepared.metrics.bytes_packed.saturating_add(pack.bytes);
                prepared.checksums.insert(pack.pack_id.clone(), pack.checksum);
                prepared.pack_files.push((pack.pack_id, pack.path));
                prepared.new_pack_entries.extend(pack.entries);
                }
                if uploads.is_empty() {
                    upload_active_ms = upload_active_ms.saturating_add(upload_started.take().map(elapsed_ms).unwrap_or(0));
                }
            }
            next = workers.receiver.recv(), if ready.is_none() && existing.is_empty() && !source_closed => {
                match next {
                    Some(result) => ready = Some(result?),
                    None => source_closed = true,
                }
            }
            _ = tick.tick() => {}
        }
    }
    // Scoped upload futures are drained before workers are joined and the lease
    // can be released. Error/cancellation drop follows the same ownership order.
    drop(uploads);
    drop(workers);
    if prepared.pack_files.len() != total_plans {
        return Err(cancellation_error());
    }
    prepared.metrics.upload_duration_ms = prepared
        .metrics
        .upload_duration_ms
        .saturating_add(upload_active_ms);
    prepared.metrics.packing_duration_ms = prepared
        .metrics
        .packing_duration_ms
        .saturating_add(elapsed_ms(started));
    prepared
        .pack_index
        .extend(prepared.new_pack_entries.iter().cloned());
    prepared
        .pack_index
        .sort_by(|left, right| left.chunk_hash.cmp(&right.chunk_hash));
    prepared
        .pack_index
        .dedup_by(|left, right| left.chunk_hash == right.chunk_hash);
    // After this durable replacement, retry needs only packs, not plaintext chunks.
    prepared.release_lease = Some(packing.lease);
    if packing.partial_capture {
        return Ok(());
    }
    write_prepared_backup(agent, master, operation, prepared)?;
    release_prepared_lease(agent, operation, prepared)
}

fn release_prepared_lease(
    agent: &StateAgent,
    operation: Uuid,
    prepared: &PreparedBackup,
) -> Result<()> {
    let Some(lease) = &prepared.release_lease else {
        return Ok(());
    };
    let cas = LocalCas::new(agent.config.paths.cache.join("cas/chunks"))?;
    cas.release_lease(lease)?;
    let max = std::env::var("NOLAND_CAS_CACHE_BYTES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1024 * 1024 * 1024);
    cas.evict_to(max)?;
    let _ = std::fs::remove_dir_all(prepared_backup_path(agent, operation).with_extension("packs"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use noland_storage::{Health, RemoteEntry, RemoteKey, RemoteMeta};

    fn root(label: &str) -> PathBuf {
        std::env::current_dir()
            .unwrap()
            .join("target/packing-tests")
            .join(format!("{label}-{}", Uuid::new_v4()))
    }

    #[test]
    fn sources_support_legacy_files_and_sealed_slices_but_reject_escapes_and_bad_bounds() {
        let root = root("sources");
        let cas = LocalCas::new(&root).unwrap();
        let lease = cas.lease().unwrap();
        let hash = noland_cas::blake3_hex(b"captured");
        let flat = lease.put_prehashed(&hash, b"captured").unwrap();
        let legacy: PackPlan = serde_json::from_value(serde_json::json!({
            "chunks": [[hash, flat.path]], "target": 128, "max": 256,
        }))
        .unwrap();
        assert!(matches!(&legacy.chunks[0].1, ChunkSource::File(_)));
        let slice = lease.capture_prehashed(&hash, b"captured").unwrap().source;
        lease.sync_all().unwrap();
        validate_lease(&cas, lease.root()).unwrap();
        for source in [&legacy.chunks[0].1, &slice] {
            assert_eq!(
                validate_source(&cas, lease.root(), &hash, source, &mut HashMap::new()).unwrap(),
                8
            );
            source.verify(&hash).unwrap();
        }
        let ChunkSource::Slice { path, offset, len } = slice else {
            panic!("expected captured slice")
        };
        let bad = ChunkSource::Slice {
            path: path.clone(),
            offset: u64::MAX,
            len,
        };
        assert!(validate_source(&cas, lease.root(), &hash, &bad, &mut HashMap::new()).is_err());
        let bad = ChunkSource::Slice {
            path: root.join("outside.capture"),
            offset,
            len,
        };
        assert!(validate_source(&cas, lease.root(), &hash, &bad, &mut HashMap::new()).is_err());
        let bad = ChunkSource::File(cas.path_for_hash(&hash).unwrap());
        assert!(validate_source(&cas, lease.root(), &hash, &bad, &mut HashMap::new()).is_err());
        let groups = group_sources(
            vec![(hash.clone(), ChunkSource::Slice { path, offset, len })],
            100,
            100,
        )
        .unwrap();
        assert_eq!(groups.len(), 1); // The whole segment's metadata is not counted as payload.
        drop(lease);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn source_validation_rejects_symlinks_at_every_lease_component() {
        use std::os::unix::fs::symlink;
        let root = root("symlinks");
        let cas = LocalCas::new(&root).unwrap();
        let lease = cas.lease().unwrap();
        let hash = noland_cas::blake3_hex(b"payload");
        let flat = lease.put_prehashed(&hash, b"payload").unwrap();
        std::fs::remove_file(&flat.path).unwrap();
        symlink(cas.path_for_hash(&hash).unwrap(), &flat.path).unwrap();
        assert!(validate_source(
            &cas,
            lease.root(),
            &hash,
            &ChunkSource::File(flat.path),
            &mut HashMap::new()
        )
        .is_err());
        let other = root.join("other");
        std::fs::create_dir_all(&other).unwrap();
        symlink(&other, lease.root().join(".segments")).unwrap();
        let slice = ChunkSource::Slice {
            path: lease.root().join(".segments/a.capture"),
            offset: 56,
            len: 7,
        };
        assert!(validate_source(&cas, lease.root(), &hash, &slice, &mut HashMap::new()).is_err());
        let fake = root.join(".leases/fake");
        symlink(&other, &fake).unwrap();
        assert!(validate_lease(&cas, &fake).is_err());
        drop(lease);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dropping_workers_joins_byte_waiters_and_blocked_result_senders() {
        let budget = ReadyBudget::new(1, 8);
        assert!(budget.acquire(9).is_err());
        let held = budget.acquire(8).unwrap();
        assert_eq!(budget.snapshot(), (1, 8));
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        assert!(sender.blocking_send(Err(cancellation_error())).is_ok());
        let completed = Arc::new(AtomicUsize::new(0));
        let mut workers = Workers {
            receiver,
            cancelled: Arc::new(AtomicBool::new(false)),
            threads: Vec::new(),
            budget: budget.clone(),
            active: Arc::new(AtomicUsize::new(0)),
        };
        let finished = completed.clone();
        workers.threads.push(std::thread::spawn(move || {
            assert!(sender.blocking_send(Err(cancellation_error())).is_err());
            finished.fetch_add(1, Ordering::SeqCst);
        }));
        let finished = completed.clone();
        let waiting = budget.clone();
        workers.threads.push(std::thread::spawn(move || {
            assert!(waiting.acquire(1).is_err());
            finished.fetch_add(1, Ordering::SeqCst);
        }));
        drop(workers);
        assert_eq!(completed.load(Ordering::SeqCst), 2);
        drop(held);
        assert_eq!(budget.snapshot(), (0, 0));
    }

    struct SlowFirstProvider {
        inner: LocalStorage,
        calls: AtomicUsize,
        lists: AtomicUsize,
        active: AtomicUsize,
        peak: AtomicUsize,
        third_started: tokio::sync::Notify,
        release_first: tokio::sync::Semaphore,
        keys: Mutex<Vec<String>>,
        groups: Mutex<Vec<usize>>,
        fail_on_call: AtomicUsize,
        upload_budget: usize,
    }

    #[async_trait]
    impl SharedStorageProvider for SlowFirstProvider {
        async fn health_check(&self) -> Result<Health> {
            self.inner.health_check().await
        }
        async fn ensure_root(&self) -> Result<()> {
            self.inner.ensure_root().await
        }
        async fn stat(&self, key: &RemoteKey) -> Result<Option<RemoteMeta>> {
            self.inner.stat(key).await
        }
        async fn upload_immutable(&self, path: &Path, key: &RemoteKey) -> Result<RemoteMeta> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            let _guard = ActiveWorker(&self.active);
            self.peak.fetch_max(active, Ordering::SeqCst);
            self.keys.lock().unwrap().push(key.as_str().into());
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if self.fail_on_call.load(Ordering::SeqCst) == call {
                return Err(StateError::Storage(
                    "injected grouped transfer failure".into(),
                ));
            }
            if call == 1 {
                self.release_first.acquire().await.unwrap().forget();
            } else if call >= 3 {
                self.third_started.notify_one();
            }
            self.inner.upload_immutable(path, key).await
        }
        async fn upload_immutable_bulk_observed(
            &self,
            uploads: &[noland_storage::ImmutableUpload],
            observer: Option<&(dyn noland_storage::ImmutableUploadObserver + '_)>,
        ) -> Result<Vec<RemoteMeta>> {
            self.groups.lock().unwrap().push(uploads.len());
            let mut output = Vec::new();
            let mut active = RollingUploadWindow::new(self.upload_budget, 4096);
            for upload in uploads {
                active.push(std::fs::metadata(&upload.local)?.len(), async move {
                    let meta = self.upload_immutable(&upload.local, &upload.key).await?;
                    Ok((upload, meta))
                })?;
            }
            while let Some(completed) = active.next().await {
                let (upload, meta) = completed?;
                if let Some(observer) = observer {
                    observer.completed(
                        std::slice::from_ref(upload),
                        noland_storage::ImmutableUploadCompletion::Uploaded,
                    )?;
                }
                output.push(meta);
            }
            Ok(output)
        }
        fn supports_parallel_upload_groups(&self) -> bool {
            true
        }
        async fn download(&self, key: &RemoteKey, path: &Path) -> Result<()> {
            self.inner.download(key, path).await
        }
        async fn list_prefix(&self, prefix: &RemoteKey) -> Result<Vec<RemoteEntry>> {
            self.lists.fetch_add(1, Ordering::SeqCst);
            self.inner.list_prefix(prefix).await
        }
        async fn put_small_versioned(&self, _: bytes::Bytes, _: &RemoteKey) -> Result<RemoteMeta> {
            panic!("packing must not publish metadata")
        }
        fn storage_identity(&self) -> Option<ProviderRootIdentity> {
            self.inner.storage_identity()
        }
        fn transfer_tuning(&self) -> TransferTuning {
            TransferTuning {
                max_parallel_uploads: self.upload_budget,
                max_bulk_files: self.upload_budget,
                max_bulk_bytes: 4096,
                ..TransferTuning::default()
            }
        }
    }

    fn pipeline_fixture() -> (
        PathBuf,
        StateAgent,
        SlowFirstProvider,
        MasterKey,
        Uuid,
        PreparedBackup,
    ) {
        let root = root("pipeline");
        let agent = StateAgent::boot(crate::AgentConfig::isolated(root.clone())).unwrap();
        let provider = SlowFirstProvider {
            inner: LocalStorage::new(root.join("remote")),
            calls: 0.into(),
            lists: 0.into(),
            active: 0.into(),
            peak: 0.into(),
            third_started: tokio::sync::Notify::new(),
            release_first: tokio::sync::Semaphore::new(0),
            keys: Mutex::default(),
            groups: Mutex::default(),
            fail_on_call: AtomicUsize::new(0),
            upload_budget: 3,
        };
        let master = MasterKey::generate();
        let operation = Uuid::new_v4();
        let app = AppIdentity::new(AppId::desktop("packing-test"), "Packing Test");
        agent.db.upsert_app(&app).unwrap();
        agent
            .db
            .upsert_operation(&OperationRecord {
                operation_id: operation,
                kind: "backup".into(),
                app_id: Some(app.app_id.clone()),
                state: "PACKING".into(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
                last_error: None,
                detail_json: serde_json::json!({}),
            })
            .unwrap();
        let cas = LocalCas::new(agent.config.paths.cache.join("cas/chunks")).unwrap();
        let lease = cas.lease().unwrap();
        let mut plans = Vec::new();
        for index in 0..6 {
            let bytes = format!("captured pack {index}").into_bytes();
            let hash = noland_cas::blake3_hex(&bytes);
            let stored = lease.put_prehashed(&hash, &bytes).unwrap();
            plans.push(PackPlan {
                chunks: vec![(hash, stored.path.into())],
                target: 128,
                max: 256,
            });
        }
        lease.sync_all().unwrap();
        let prepared = PreparedBackup {
            version: 1,
            storage_id: provider
                .storage_identity()
                .map(|identity| identity.cache_key()),
            manifest: BundleManifest::new(
                ManifestApp::from(&app),
                ManifestSource {
                    instance_id: agent.config.instance_id,
                    image_id: agent.config.image_id.clone(),
                    captured_at: Utc::now(),
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
            release_lease: None,
            capture: None,
            release_capture: None,
            packing: Some(PackingState {
                plans,
                lease: lease.root().to_path_buf(),
                workers: 1,
                receipt_prefix: String::new(),
                partial_capture: false,
                upload_slots: None,
            }),
        };
        write_prepared_backup(&agent, &master, operation, &prepared).unwrap();
        lease.retain_for_retry();
        (root, agent, provider, master, operation, prepared)
    }

    #[tokio::test]
    async fn ready_packs_group_without_exceeding_slots_and_partial_failure_reuses_receipts() {
        let (root, agent, mut provider, master, operation, mut prepared) = pipeline_fixture();
        provider.upload_budget = 4;
        let directory = agent
            .config
            .paths
            .packs
            .join(prepared.manifest.bundle_id.to_string());
        // Presealed retry receipts make readiness deterministic, independent of
        // compressor scheduling. The upload path must opportunistically group.
        for (index, plan) in prepared.packing.as_ref().unwrap().plans.iter().enumerate() {
            let pack = pack_chunk_files_with_limits(
                &directory,
                &master,
                plan.chunks.clone(),
                |_| false,
                plan.target,
                plan.max,
            )
            .unwrap()
            .remove(0);
            write_receipt(
                &receipt_path(&agent, operation, index),
                &master,
                &receipt_aad(operation, index),
                &pack,
            )
            .unwrap();
        }
        provider.release_first.add_permits(1);
        provider.fail_on_call.store(2, Ordering::SeqCst);
        assert!(
            pack_and_upload(&agent, &provider, &master, operation, &mut prepared, true)
                .await
                .is_err()
        );
        assert_eq!(provider.groups.lock().unwrap()[0], 2);
        let completed = provider.keys.lock().unwrap()[0].clone();
        assert!(agent
            .db
            .sync_journal_completed(operation, &completed)
            .unwrap());
        let mut retry = read_prepared_backup(
            &agent,
            &provider,
            &master,
            operation,
            &prepared.manifest.app.app_id,
            BackupMode::PersonalState,
        )
        .unwrap()
        .unwrap();
        pack_and_upload(&agent, &provider, &master, operation, &mut retry, true)
            .await
            .unwrap();
        assert_eq!(retry.pack_files.len(), 6);
        assert_eq!(
            provider
                .keys
                .lock()
                .unwrap()
                .iter()
                .filter(|key| **key == completed)
                .count(),
            1
        );
        assert!(provider
            .groups
            .lock()
            .unwrap()
            .iter()
            .all(|count| *count <= 2));
        drop(agent);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn rolling_pipeline_refills_past_a_slow_pack_with_one_compressor() {
        let (root, agent, provider, master, operation, mut prepared) = pipeline_fixture();
        let release = async {
            provider.third_started.notified().await;
            assert!(provider.active.load(Ordering::SeqCst) >= 1);
            assert!(!agent
                .db
                .sync_journal_completed(operation, &provider.keys.lock().unwrap()[0])
                .unwrap());
            provider.release_first.add_permits(1);
        };
        let run = pack_and_upload(&agent, &provider, &master, operation, &mut prepared, false);
        tokio::time::timeout(Duration::from_secs(10), async {
            let (result, ()) = tokio::join!(run, release);
            result.unwrap();
        })
        .await
        .unwrap();
        assert_eq!(prepared.pack_files.len(), 6);
        assert_eq!(provider.lists.load(Ordering::SeqCst), 1);
        assert!(provider.peak.load(Ordering::SeqCst) >= 2);
        assert!(provider.peak.load(Ordering::SeqCst) <= 3);
        let progress = agent.db.get_operation_progress(operation).unwrap().unwrap();
        assert_eq!(
            progress.detail_json["pack_pipeline"]["compression_workers"],
            1
        );
        assert_eq!(progress.detail_json["pack_pipeline"]["inflight_count"], 0);
        assert!(
            progress.detail_json["pack_pipeline"]["peak_inflight_bytes"]
                .as_u64()
                .unwrap()
                <= 4096
        );
        let summary = agent
            .db
            .sync_journal_summary_for_kind(operation, ContentObjectKind::Pack)
            .unwrap();
        assert!(summary.completed_bytes > 0);
        // Completed retry ciphertext need not exist locally. Journal totals and
        // the completed pipeline detail must survive the transition to commit.
        for (_, path) in &prepared.pack_files {
            std::fs::remove_file(path).unwrap();
        }
        assert!(report_commit_progress(&agent, operation, &prepared.pack_files, true).unwrap());
        let progress = agent.db.get_operation_progress(operation).unwrap().unwrap();
        assert_eq!(progress.phase, "committing");
        assert_eq!(progress.completed_units, summary.completed_bytes);
        assert_eq!(progress.total_units, Some(summary.total_bytes));
        assert_eq!(
            progress.detail_json["bytes_transferred"],
            summary.bytes_transferred
        );
        assert_eq!(
            progress.detail_json["pack_pipeline"]["phase"],
            "transfers_complete"
        );
        drop(agent);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn resumed_sealing_rejects_corrupt_capture_before_receipt_or_upload() {
        let (root, agent, provider, master, operation, mut prepared) = pipeline_fixture();
        let source = prepared.packing.as_ref().unwrap().plans[0].chunks[0]
            .1
            .path()
            .to_path_buf();
        let original = std::fs::read(&source).unwrap();
        let mut corrupt = original.clone();
        corrupt[0] ^= 1;
        std::fs::write(&source, corrupt).unwrap();
        assert!(
            pack_and_upload(&agent, &provider, &master, operation, &mut prepared, true)
                .await
                .is_err()
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert!(!receipt_path(&agent, operation, 0).exists());
        std::fs::write(source, original).unwrap();
        let mut retry = read_prepared_backup(
            &agent,
            &provider,
            &master,
            operation,
            &prepared.manifest.app.app_id,
            BackupMode::PersonalState,
        )
        .unwrap()
        .unwrap();
        provider.release_first.add_permits(1);
        pack_and_upload(&agent, &provider, &master, operation, &mut retry, true)
            .await
            .unwrap();
        assert_eq!(retry.pack_files.len(), 6);
        drop(agent);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_raw_resume_checkpoint_is_still_authenticated_and_readable() {
        let (root, agent, provider, master, operation, prepared) = pipeline_fixture();
        let legacy = noland_crypto::wrap_envelope(
            &noland_crypto::derive_keys(&master).manifest,
            format!("backup-resume:{operation}").as_bytes(),
            &serde_json::to_vec(&prepared).unwrap(),
        )
        .unwrap();
        std::fs::write(prepared_backup_path(&agent, operation), legacy).unwrap();
        let loaded = read_prepared_backup(
            &agent,
            &provider,
            &master,
            operation,
            &prepared.manifest.app.app_id,
            BackupMode::PersonalState,
        )
        .unwrap()
        .unwrap();
        assert_eq!(loaded.manifest.bundle_id, prepared.manifest.bundle_id);
        assert_eq!(loaded.packing.unwrap().plans.len(), 6);
        assert!(read_prepared_backup(
            &agent,
            &provider,
            &MasterKey::generate(),
            operation,
            &prepared.manifest.app.app_id,
            BackupMode::PersonalState
        )
        .is_err());
        drop(agent);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn cancelled_pipeline_joins_uploads_and_retry_reuses_completed_pack_receipts() {
        let (root, agent, provider, master, operation, mut prepared) = pipeline_fixture();
        let mut run = Box::pin(pack_and_upload(
            &agent,
            &provider,
            &master,
            operation,
            &mut prepared,
            false,
        ));
        tokio::time::timeout(Duration::from_secs(10), async {
            tokio::select! {
                result = run.as_mut() => panic!("slow first upload unexpectedly completed: {result:?}"),
                _ = provider.third_started.notified() => {}
            }
        }).await.unwrap();
        drop(run);
        assert_eq!(provider.active.load(Ordering::SeqCst), 0);
        let first_key = provider.keys.lock().unwrap()[0].clone();
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
        let plans = resumed.packing.as_ref().unwrap().plans.len();
        let mut completed = Vec::new();
        for index in 0..plans {
            if let Some(pack) = read_receipt(
                &receipt_path(&agent, operation, index),
                &master,
                &receipt_aad(operation, index),
            )
            .unwrap()
            {
                let key = pack_key(&pack.pack_id);
                if agent.db.sync_journal_completed(operation, &key).unwrap() {
                    std::fs::remove_file(pack.path).unwrap();
                    completed.push(key);
                }
            }
        }
        assert!(!completed.is_empty());
        pack_and_upload(&agent, &provider, &master, operation, &mut resumed, true)
            .await
            .unwrap();
        assert_eq!(resumed.pack_files.len(), plans);
        let keys = provider.keys.lock().unwrap();
        assert_eq!(keys.iter().filter(|key| **key == first_key).count(), 2);
        for key in completed {
            assert_eq!(keys.iter().filter(|seen| **seen == key).count(), 1);
        }
        assert_eq!(provider.lists.load(Ordering::SeqCst), 2); // A new operation attempt refreshes inventory once.
        drop(keys);
        drop(agent);
        std::fs::remove_dir_all(root).unwrap();
    }
}
