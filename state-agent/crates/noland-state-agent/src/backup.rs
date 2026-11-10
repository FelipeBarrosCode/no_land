use std::collections::{BTreeMap, BTreeSet, HashMap};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use noland_cas::{chunk_file_streaming, CasLease, LocalCas};
use noland_classifier::Classifier;
use noland_crypto::MasterKey;
use noland_pack::{pack_chunk_files_with_limits, BuiltPack, PackIndexEntry};
use noland_rclone_adapter::{
    EphemeralRcloneSession, ProviderKind, ProviderRootIdentity, TransferTuning, TransferWorkload,
};
use noland_snapshot::create_view;
use noland_state_core::*;
use noland_storage::{
    commit_bundle_with_index_for_operation, read_committed_manifest, read_pack_index,
    update_catalog_with_bundle, write_guarded_ephemeral_session, LocalStorage, RcloneStorage,
    SharedStorageProvider,
};
use uuid::Uuid;

use crate::reconcile::{
    known_install_roots, reconcile_app, reconcile_app_for_complete_backup, steam_appmanifest_path,
    SteamBackupScope,
};
use crate::StateAgent;

mod packing;
#[cfg(test)]
mod scale;

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u64::MAX as u128) as u64
}

const PROGRESS_FLUSH_INTERVAL: Duration = Duration::from_millis(500);
const PROGRESS_FLUSH_FILES: u64 = 384;
const PROGRESS_FLUSH_BYTES: u64 = 96 * 1024 * 1024;

struct ProgressReporter {
    operation_id: Uuid,
    completed_files: u64,
    completed_bytes: u64,
    last_flushed_files: u64,
    last_flushed_bytes: u64,
    last_flush: Instant,
}

impl ProgressReporter {
    fn new(operation_id: Uuid) -> Self {
        Self {
            operation_id,
            completed_files: 0,
            completed_bytes: 0,
            last_flushed_files: 0,
            last_flushed_bytes: 0,
            last_flush: Instant::now(),
        }
    }

    fn record_file(&mut self, bytes: u64) {
        self.completed_files = self.completed_files.saturating_add(1);
        self.completed_bytes = self.completed_bytes.saturating_add(bytes);
    }

    fn should_flush(&self) -> bool {
        self.last_flush.elapsed() >= PROGRESS_FLUSH_INTERVAL
            || self.completed_files.saturating_sub(self.last_flushed_files) >= PROGRESS_FLUSH_FILES
            || self.completed_bytes.saturating_sub(self.last_flushed_bytes) >= PROGRESS_FLUSH_BYTES
    }

    fn maybe_flush(
        &mut self,
        agent: &StateAgent,
        progress: &mut OperationProgress,
        metrics: &OperationMetrics,
    ) -> Result<()> {
        if self.should_flush() {
            self.flush(agent, progress, metrics)?;
        }
        Ok(())
    }

    fn flush(
        &mut self,
        agent: &StateAgent,
        progress: &mut OperationProgress,
        metrics: &OperationMetrics,
    ) -> Result<()> {
        progress.completed_units = self.completed_files;
        progress.detail_json = serde_json::json!({
            "bytes_hashed": metrics.bytes_hashed,
            "files_rehashed": metrics.num_files_rehashed,
            "bytes_completed": self.completed_bytes,
        });
        progress.updated_at = Utc::now();
        agent
            .db
            .set_operation_progress(self.operation_id, Some(progress))?;
        self.last_flushed_files = self.completed_files;
        self.last_flushed_bytes = self.completed_bytes;
        self.last_flush = Instant::now();
        Ok(())
    }
}

struct RemoteChunkIndex {
    entries: HashMap<String, PackIndexEntry>,
}

#[derive(Debug)]
struct HashJob {
    app_id: AppId,
    source: PathBuf,
    snapshot_root: PathBuf,
    staged: Option<PathBuf>,
    record: PathRecord,
    association: PathAssociation,
    logical: LogicalPath,
    shared_app_ids: Vec<AppId>,
}

#[derive(Debug, Default)]
struct HashMetricsDelta {
    bytes_hashed: u64,
    chunks_reused: u64,
    chunks_created: u64,
    local_cas_hits: u64,
    remote_index_hits: u64,
    bytes_reused_local: u64,
}

#[derive(Debug)]
struct HashResult {
    snapshot_duration_ms: u64,
    consistency: ConsistencyKind,
    manifest_file: ManifestFile,
    file_state: FileStateRecord,
    new_chunks: Vec<(String, PathBuf)>,
    remote_entries: Vec<PackIndexEntry>,
    cas_observations: Vec<LocalCasEntry>,
    small_file_chunk_hashes: Vec<String>,
    metrics: HashMetricsDelta,
}

impl RemoteChunkIndex {
    fn empty() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    fn get(&self, hash: &str) -> Option<&PackIndexEntry> {
        self.entries.get(hash)
    }
}

fn hashing_worker_count(performance: BackupPerformanceMode) -> usize {
    if let Ok(value) = std::env::var("NOLAND_HASH_WORKERS") {
        if let Ok(value) = value.parse::<usize>() {
            return value.clamp(1, 8);
        }
    }
    let available = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(4);
    match performance {
        BackupPerformanceMode::Balanced => 4.min(available).max(1),
        BackupPerformanceMode::Fast | BackupPerformanceMode::Full => 8.min(available).max(1),
    }
}

fn pack_worker_count(performance: BackupPerformanceMode) -> usize {
    if let Ok(value) = std::env::var("NOLAND_PACK_WORKERS") {
        if let Ok(value) = value.parse::<usize>() {
            return value.clamp(1, 4);
        }
    }
    match performance {
        BackupPerformanceMode::Balanced => 2,
        BackupPerformanceMode::Fast | BackupPerformanceMode::Full => 4,
    }
}

type CancellationCheck = Arc<dyn Fn() -> bool + Send + Sync>;

fn cancellation_error() -> StateError {
    StateError::Invalid("backup cancellation requested".into())
}

fn plan_pack_chunks(
    chunks: Vec<(String, PathBuf)>,
    target: u64,
    max: u64,
) -> Result<Vec<Vec<(String, PathBuf)>>> {
    const PACK_HEADER_BYTES: u64 = 24;
    const RECORD_OVERHEAD_BYTES: u64 = 52;
    if target == 0 || target > max {
        return Err(StateError::Invalid("invalid pack size limits".into()));
    }
    // Input follows launch tier, file path, and file offset. Hash ordering scatters
    // one file across packs and forces a full readback during restore.
    let mut plans = Vec::new();
    let mut current = Vec::new();
    let mut current_bytes = PACK_HEADER_BYTES;
    for (hash, path) in chunks {
        let payload_bytes = std::fs::metadata(&path)?.len();
        let record_bytes = RECORD_OVERHEAD_BYTES
            .checked_add(payload_bytes)
            .ok_or_else(|| StateError::msg("chunk length exceeds pack format"))?;
        if PACK_HEADER_BYTES
            .checked_add(record_bytes)
            .is_none_or(|bytes| bytes > max)
        {
            return Err(StateError::msg("chunk exceeds pack hard maximum"));
        }
        if !current.is_empty()
            && (current_bytes >= target
                || current_bytes
                    .checked_add(record_bytes)
                    .is_none_or(|bytes| bytes > max))
        {
            plans.push(std::mem::take(&mut current));
            current_bytes = PACK_HEADER_BYTES;
        }
        current_bytes += record_bytes;
        current.push((hash, path));
    }
    if !current.is_empty() {
        plans.push(current);
    }
    Ok(plans)
}

#[cfg(test)]
fn build_packs_parallel(
    dest_dir: &Path,
    master: &MasterKey,
    chunks: Vec<(String, PathBuf)>,
    target: u64,
    max: u64,
    worker_count: usize,
    cancelled: CancellationCheck,
) -> Result<Vec<BuiltPack>> {
    let plans = plan_pack_chunks(chunks, target, max)?;
    if plans.is_empty() {
        return Ok(Vec::new());
    }
    if plans.len() == 1 || worker_count <= 1 {
        let mut built = Vec::with_capacity(plans.len());
        for plan in plans {
            if cancelled() {
                return Err(cancellation_error());
            }
            built.extend(pack_chunk_files_with_limits(
                dest_dir,
                master,
                plan,
                |_| false,
                target,
                max,
            )?);
        }
        return Ok(built);
    }

    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    };
    let workers = worker_count.min(plans.len()).max(1);
    let (plan_tx, plan_rx) = mpsc::sync_channel::<Vec<(String, PathBuf)>>(workers * 2);
    let (result_tx, result_rx) = mpsc::sync_channel::<Result<BuiltPack>>(workers * 2);
    let plan_rx = Arc::new(Mutex::new(plan_rx));
    let stopped = Arc::new(AtomicBool::new(false));
    let mut built = Vec::with_capacity(plans.len());
    let mut first_error = None;
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let plan_rx = Arc::clone(&plan_rx);
            let result_tx = result_tx.clone();
            let cancelled = Arc::clone(&cancelled);
            let stopped = Arc::clone(&stopped);
            scope.spawn(move || loop {
                let plan = match plan_rx.lock().expect("pack plan receiver poisoned").recv() {
                    Ok(plan) => plan,
                    Err(_) => break,
                };
                if cancelled() {
                    stopped.store(true, Ordering::Release);
                    let _ = result_tx.send(Err(cancellation_error()));
                    break;
                }
                if stopped.load(Ordering::Acquire) {
                    break;
                }
                let result =
                    pack_chunk_files_with_limits(dest_dir, master, plan, |_| false, target, max)
                        .and_then(|mut packs| {
                            packs
                                .pop()
                                .ok_or_else(|| StateError::msg("pack plan produced no pack"))
                        });
                let result = if cancelled() {
                    Err(cancellation_error())
                } else {
                    result
                };
                let failed = result.is_err();
                if failed {
                    stopped.store(true, Ordering::Release);
                }
                if result_tx.send(result).is_err() {
                    break;
                }
                if failed {
                    break;
                }
            });
        }
        drop(result_tx);
        drop(plan_rx);
        let feeder_cancelled = Arc::clone(&cancelled);
        let feeder_stopped = Arc::clone(&stopped);
        let feeder = scope.spawn(move || {
            for plan in plans {
                if feeder_cancelled()
                    || feeder_stopped.load(Ordering::Acquire)
                    || plan_tx.send(plan).is_err()
                {
                    break;
                }
            }
        });
        for result in result_rx {
            match result {
                Ok(pack) => built.push(pack),
                Err(error) if first_error.is_none() => first_error = Some(error),
                Err(_) => {}
            }
        }
        feeder
            .join()
            .map_err(|_| StateError::msg("pack plan feeder panicked"))?;
        Ok::<(), StateError>(())
    })?;
    if let Some(error) = first_error {
        return Err(error);
    }
    built.sort_by(|left, right| left.entries[0].chunk_hash.cmp(&right.entries[0].chunk_hash));
    Ok(built)
}

fn hash_one_file(
    job: HashJob,
    cas: &CasLease,
    inherited_hashes: &std::collections::HashSet<String>,
    remote_chunks: &RemoteChunkIndex,
    observed_at: chrono::DateTime<Utc>,
) -> Result<HashResult> {
    let snapshot_started = Instant::now();
    let source_before = std::fs::metadata(&job.source)?;
    let view = if job.staged.is_none() {
        Some(create_view(
            &job.snapshot_root,
            std::slice::from_ref(&job.source),
            false,
        )?)
    } else {
        None
    };
    let _cleanup = view.as_ref().map(|view| SnapshotCleanup(view.root.clone()));
    let staged = job
        .staged
        .as_ref()
        .or_else(|| {
            view.as_ref()
                .and_then(|view| view.mappings.first().map(|mapping| &mapping.staged))
        })
        .ok_or_else(|| {
            StateError::Invalid("backup source disappeared while snapshotting".into())
        })?;
    let snapshot_duration_ms = elapsed_ms(snapshot_started);
    let mut new_chunks = Vec::new();
    let mut remote_entries = Vec::new();
    let mut cas_observations = Vec::new();
    let mut metrics = HashMetricsDelta::default();
    let summary = chunk_file_streaming(staged, |chunk, payload| {
        if inherited_hashes.contains(&chunk.hash) {
            metrics.chunks_reused = metrics.chunks_reused.saturating_add(1);
            return Ok(());
        }
        if let Some(remote_entry) = remote_chunks.get(&chunk.hash) {
            metrics.remote_index_hits = metrics.remote_index_hits.saturating_add(1);
            metrics.chunks_reused = metrics.chunks_reused.saturating_add(1);
            remote_entries.push(remote_entry.clone());
            return Ok(());
        }
        let stored = cas.put_prehashed(&chunk.hash, payload)?;
        cas_observations.push(LocalCasEntry {
            object_kind: ContentObjectKind::Chunk,
            content_hash: chunk.hash.clone(),
            local_path: cas.cache_path(&chunk.hash)?.to_string_lossy().into_owned(),
            size: stored.bytes,
            created_at: observed_at,
            verified_at: Some(observed_at),
            last_accessed_at: observed_at,
        });
        if stored.reused {
            metrics.local_cas_hits = metrics.local_cas_hits.saturating_add(1);
            metrics.bytes_reused_local = metrics.bytes_reused_local.saturating_add(stored.bytes);
        } else {
            metrics.chunks_created = metrics.chunks_created.saturating_add(1);
        }
        new_chunks.push((chunk.hash.clone(), stored.path));
        Ok(())
    })?;

    let metadata = std::fs::metadata(&job.source)?;
    let current_mtime_ns = metadata_mtime_ns(&metadata);
    let mut current_record = job.record.clone();
    current_record.size = Some(summary.size.min(i64::MAX as u64) as i64);
    current_record.mtime_ns = current_mtime_ns;
    current_record.content_hash = Some(summary.file_hash.clone());
    #[cfg(unix)]
    {
        current_record.inode = Some(metadata.ino().min(i64::MAX as u64) as i64);
        current_record.mode = Some(metadata.mode() as i64);
        current_record.uid = Some(metadata.uid() as i64);
        current_record.gid = Some(metadata.gid() as i64);
    }
    let manifest_file = ManifestFile {
        logical_root: job.logical.logical_root.as_token(),
        relative_path: job.logical.relative_path.clone(),
        source_path_hint: Some(job.record.canonical_path.clone()),
        file_type: job
            .record
            .file_type
            .clone()
            .unwrap_or_else(|| "file".into()),
        size: summary.size,
        file_hash: summary.file_hash.clone(),
        chunks: summary.chunks,
        mode: current_record
            .mode
            .and_then(|value| u32::try_from(value).ok()),
        mtime_ns: current_record.mtime_ns,
        uid: current_record
            .uid
            .and_then(|value| u32::try_from(value).ok()),
        gid: current_record
            .gid
            .and_then(|value| u32::try_from(value).ok()),
        symlink_target: None,
        persistence_class: job.association.persistence_class,
        semantic_role: job.association.semantic_role,
        association_confidence: job.association.confidence,
        shared_app_ids: job.shared_app_ids,
    };
    metrics.bytes_hashed = summary.size;
    let mut file_state = file_state_from_manifest(&job.app_id, &manifest_file, &current_record);
    if job.record.size != Some(metadata.len().min(i64::MAX as u64) as i64)
        || job.record.mtime_ns != current_mtime_ns
        || job.record.inode != current_record.inode
        || !noland_snapshot::same_file_identity(&source_before, &metadata)
        || view
            .as_ref()
            .is_some_and(|view| view.consistency == ConsistencyKind::BestEffort)
    {
        file_state.trust = FileStateTrust::VerifyRequired;
    }
    let small_file_chunk_hashes = if noland_cas::is_small_file(summary.size) {
        manifest_file
            .chunks
            .iter()
            .map(|chunk| chunk.hash.clone())
            .collect()
    } else {
        Vec::new()
    };
    Ok(HashResult {
        snapshot_duration_ms,
        consistency: view
            .as_ref()
            .map_or(ConsistencyKind::Snapshot, |view| view.consistency),
        manifest_file,
        file_state,
        new_chunks,
        remote_entries,
        cas_observations,
        small_file_chunk_hashes,
        metrics,
    })
}

fn hash_files_parallel(
    jobs: Vec<HashJob>,
    worker_count: usize,
    cas: Arc<CasLease>,
    inherited_hashes: std::collections::HashSet<String>,
    remote_chunks: RemoteChunkIndex,
    observed_at: chrono::DateTime<Utc>,
    cancelled: CancellationCheck,
    mut on_result: impl FnMut(HashResult) -> Result<()>,
) -> Result<()> {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    };

    let worker_count = worker_count.max(1);
    let (job_tx, job_rx) = mpsc::sync_channel::<HashJob>(worker_count * 2);
    let (result_tx, result_rx) = mpsc::sync_channel::<Result<HashResult>>(worker_count * 2);
    let job_rx = Arc::new(Mutex::new(job_rx));
    let inherited_hashes = Arc::new(inherited_hashes);
    let remote_chunks = Arc::new(remote_chunks);
    let stopped = Arc::new(AtomicBool::new(false));
    let feeder_cancelled = Arc::clone(&cancelled);
    let feeder_stopped = Arc::clone(&stopped);
    let feeder = std::thread::spawn(move || {
        for job in jobs {
            if feeder_cancelled()
                || feeder_stopped.load(Ordering::Acquire)
                || job_tx.send(job).is_err()
            {
                break;
            }
        }
    });

    // The coordinator drains results while the feeder submits jobs, keeping both channels bounded.
    let mut first_error = None;
    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            let job_rx = Arc::clone(&job_rx);
            let result_tx = result_tx.clone();
            let inherited_hashes = Arc::clone(&inherited_hashes);
            let remote_chunks = Arc::clone(&remote_chunks);
            let cancelled = Arc::clone(&cancelled);
            let stopped = Arc::clone(&stopped);
            let cas = cas.clone();
            scope.spawn(move || loop {
                let job = match job_rx.lock().expect("hash job receiver poisoned").recv() {
                    Ok(job) => job,
                    Err(_) => break,
                };
                if stopped.load(Ordering::Acquire) {
                    break;
                }
                let result = if cancelled() {
                    Err(cancellation_error())
                } else {
                    hash_one_file(job, &cas, &inherited_hashes, &remote_chunks, observed_at)
                };
                let failed = result.is_err();
                if failed {
                    stopped.store(true, Ordering::Release);
                }
                if result_tx.send(result).is_err() {
                    break;
                }
                if failed || cancelled() {
                    break;
                }
            });
        }
        drop(result_tx);
        drop(job_rx);
        for result in result_rx {
            match result {
                Ok(result) if first_error.is_none() => {
                    if let Err(error) = on_result(result) {
                        stopped.store(true, Ordering::Release);
                        first_error = Some(error);
                    }
                }
                Ok(_) => {}
                Err(error) if first_error.is_none() => first_error = Some(error),
                Err(_) => {}
            }
        }
        Ok::<(), StateError>(())
    })?;
    feeder.join().expect("hash job feeder panicked");
    if let Some(error) = first_error {
        return Err(error);
    }
    Ok(())
}

struct SnapshotCleanup(PathBuf);

impl Drop for SnapshotCleanup {
    fn drop(&mut self) {
        let _ = noland_snapshot::discard_root(&self.0);
    }
}

struct PackCleanup {
    path: PathBuf,
    cloud_committed: bool,
}

impl PackCleanup {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            cloud_committed: false,
        }
    }

    fn mark_cloud_committed(&mut self) {
        self.cloud_committed = true;
    }
}

impl Drop for PackCleanup {
    fn drop(&mut self) {
        if self.cloud_committed {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

fn operation_metrics(op: &OperationRecord) -> OperationMetrics {
    op.detail_json
        .get("metrics")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

fn persist_operation(
    agent: &StateAgent,
    op: &mut OperationRecord,
    state: BackupState,
    metrics: &OperationMetrics,
) -> Result<()> {
    op.state = state.as_str().into();
    op.updated_at = Utc::now();
    let detail = op
        .detail_json
        .as_object_mut()
        .expect("operation detail is always an object");
    detail.insert("metrics".into(), serde_json::to_value(metrics)?);
    agent.db.upsert_operation(op)?;
    agent.db.set_operation_metrics(op.operation_id, metrics)?;
    tracing::info!(
        operation_id = %op.operation_id,
        app_id = op.app_id.as_ref().map(AppId::as_str),
        state = state.as_str(),
        metrics = %serde_json::to_string(metrics).unwrap_or_default(),
        "backup operation advanced"
    );
    Ok(())
}

pub async fn run_backup(
    agent: &StateAgent,
    app_id: &AppId,
    mode: BackupMode,
    performance: BackupPerformanceMode,
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    operation_id: Option<Uuid>,
) -> Result<BundleManifest> {
    let total_started = Instant::now();
    let op_id = operation_id.unwrap_or_else(Uuid::new_v4);
    let mut op = agent
        .db
        .get_operation(op_id)?
        .unwrap_or_else(|| OperationRecord {
            operation_id: op_id,
            kind: "backup".into(),
            app_id: Some(app_id.clone()),
            state: BackupState::Queued.as_str().into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            last_error: None,
            detail_json: serde_json::json!({}),
        });
    if !op.detail_json.is_object() {
        op.detail_json = serde_json::json!({});
    }
    let mut metrics = operation_metrics(&op);
    if let Some(prepared) = read_prepared_backup(agent, provider, master, op_id, app_id, mode)? {
        return finish_prepared_backup(agent, provider, master, op, prepared, total_started, true)
            .await;
    }
    persist_operation(agent, &mut op, BackupState::Discovering, &metrics)?;

    let identity = agent
        .db
        .get_app(app_id)?
        .ok_or_else(|| StateError::NotFound(app_id.to_string()))?;
    let captured_generation = agent.db.capture_generation(app_id)?;
    let dirty_state = agent
        .db
        .list_dirty_apps()?
        .into_iter()
        .find(|dirty| dirty.app_id == *app_id);
    let pending_mutations = agent.db.pending_app_mutations(app_id, 10_000)?;
    let dirty_path_ids = dirty_state
        .as_ref()
        .map(|state| {
            state
                .dirty_paths
                .iter()
                .copied()
                .collect::<std::collections::HashSet<_>>()
        })
        .unwrap_or_default();
    let mutated_paths = pending_mutations
        .iter()
        .map(|mutation| mutation.path.as_str())
        .collect::<std::collections::HashSet<_>>();
    metrics.num_dirty_paths = dirty_state
        .as_ref()
        .map(|dirty| dirty.dirty_paths.len() as u64)
        .unwrap_or(0);
    metrics.num_dirty_roots = agent.db.list_dirty_roots(Some(app_id))?.len() as u64;

    let parent = load_parent(provider, master, agent, app_id).await;
    let parent_scope = parent
        .as_ref()
        .map(|(manifest, _)| (manifest.mode, manifest.files.len()));
    if parent.is_some()
        && dirty_state.is_none()
        && pending_mutations.is_empty()
        && mode == BackupMode::PersonalState
        && performance != BackupPerformanceMode::Full
    {
        let (parent_manifest, _) = parent.expect("checked parent");
        metrics.num_candidate_paths = 0;
        metrics.num_files_skipped_fast_identity = parent_manifest.files.len() as u64;
        metrics.total_duration_ms =
            elapsed_ms(total_started).saturating_add(metrics.discovery_duration_ms);
        mark_noop_completed(agent, &mut op, &mut metrics, &parent_manifest)?;
        return Ok(parent_manifest);
    }

    let requires_reconciliation = dirty_state
        .as_ref()
        .is_some_and(|dirty| dirty.requires_reconciliation)
        || agent
            .db
            .list_dirty_roots(Some(app_id))?
            .iter()
            .any(|root| root.requires_reconciliation);
    if requires_reconciliation || mode == BackupMode::CompleteApplication {
        persist_operation(agent, &mut op, BackupState::Reconciling, &metrics)?;
        let started = Instant::now();
        let reconciled = if mode == BackupMode::CompleteApplication {
            reconcile_app_for_complete_backup(agent, app_id)?
        } else {
            reconcile_app(agent, app_id)?
        };
        metrics.reconciliation_duration_ms = elapsed_ms(started);
        metrics.num_files_scanned = reconciled as u64;
    }

    let planning_started = Instant::now();
    let classifier = Classifier::new(&agent.db, &agent.config.image_id)
        .with_exclusion_context(agent.config.paths.clone(), agent.config.home.clone());
    let roots = agent.roots.lock().clone();
    let strict_steam_scope = if mode == BackupMode::CompleteApplication {
        SteamBackupScope::load(agent, app_id)?
    } else {
        None
    };
    let install_roots = if mode == BackupMode::CompleteApplication {
        strict_steam_scope
            .as_ref()
            .map(|scope| scope.install_roots().to_vec())
            .unwrap_or(known_install_roots(agent, app_id)?)
    } else {
        Vec::new()
    };
    let mut candidates = BTreeMap::<String, (PathRecord, PathAssociation)>::new();
    let full_scope = parent.is_none()
        || mode == BackupMode::CompleteApplication
        || performance == BackupPerformanceMode::Full
        || requires_reconciliation;

    if full_scope {
        classifier.reclassify_app(app_id)?;
        let rows = if mode == BackupMode::CompleteApplication {
            agent.db.associations_for_app(app_id)?
        } else {
            agent.db.likely_backup_associations(app_id)?
        };
        for (record, association) in rows {
            if strict_steam_scope
                .as_ref()
                .is_some_and(|scope| !scope.contains(Path::new(&record.canonical_path)))
            {
                continue;
            }
            candidates.insert(record.canonical_path.clone(), (record, association));
        }
    } else {
        let mut candidate_records = BTreeMap::<String, PathRecord>::new();
        for mutation in &pending_mutations {
            if let Some(record) = agent.db.get_path_by_canonical(&mutation.path)? {
                candidate_records.insert(record.canonical_path.clone(), record);
            }
        }
        if let Some(dirty) = &dirty_state {
            for path_id in &dirty.dirty_paths {
                if let Some(record) = agent.db.get_path_by_id(*path_id)? {
                    candidate_records.insert(record.canonical_path.clone(), record);
                }
            }
        }
        let path_ids = candidate_records
            .values()
            .map(|record| record.path_id)
            .collect::<Vec<_>>();
        let associations_by_path = agent.db.associations_for_paths(&path_ids)?;
        for (canonical, record) in candidate_records {
            if let Some(association) = associations_by_path
                .get(&record.path_id)
                .and_then(|associations| {
                    associations
                        .iter()
                        .find(|association| association.app_id == *app_id)
                })
                .cloned()
            {
                candidates.insert(canonical, (record, association));
            }
        }
        if candidates.is_empty() && dirty_state.is_some() {
            for (record, association) in agent.db.likely_backup_associations(app_id)? {
                candidates.insert(record.canonical_path.clone(), (record, association));
            }
        }
    }

    if let Some((record, association)) =
        track_steam_appmanifest(agent, &identity, &roots, &install_roots)?
    {
        candidates.insert(record.canonical_path.clone(), (record, association));
    }
    metrics.num_candidate_paths = candidates.len() as u64;

    let (mut manifest, mut pack_index) = match parent {
        Some((parent_manifest, parent_index)) => {
            let mut next = BundleManifest::new(
                ManifestApp::from(&identity),
                ManifestSource {
                    instance_id: agent.config.instance_id,
                    image_id: agent.config.image_id.clone(),
                    captured_at: Utc::now(),
                },
                mode,
            );
            next.parent_bundle_id = Some(parent_manifest.bundle_id);
            next.files = parent_manifest.files;
            (next, parent_index)
        }
        None => (
            BundleManifest::new(
                ManifestApp::from(&identity),
                ManifestSource {
                    instance_id: agent.config.instance_id,
                    image_id: agent.config.image_id.clone(),
                    captured_at: Utc::now(),
                },
                mode,
            ),
            Vec::new(),
        ),
    };

    // One indexed lookup/removal per candidate, rather than find + retain over
    // the entire inherited manifest for each of hundreds of thousands of paths.
    let mut inherited_files = std::mem::take(&mut manifest.files)
        .into_iter()
        .map(|file| {
            (
                (file.logical_root.clone(), file.relative_path.clone()),
                file,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut tombstones = BTreeMap::<(String, String), String>::new();
    if let Some(scope) = strict_steam_scope.as_ref() {
        let previous = std::mem::take(&mut inherited_files);
        for (key, file) in previous {
            let in_scope = manifest_source_path(&file, &roots)
                .as_deref()
                .is_some_and(|path| scope.contains(path));
            if in_scope {
                inherited_files.insert(key, file);
            } else {
                tombstones.insert(
                    (file.logical_root, file.relative_path),
                    "outside strict Steam backup scope".into(),
                );
            }
        }
    }
    for mutation in &pending_mutations {
        if mutation.kind == AppMutationKind::Delete {
            if let Some(logical) = roots.classify(Path::new(&mutation.path)) {
                inherited_files.remove(&(
                    logical.logical_root.as_token(),
                    logical.relative_path.clone(),
                ));
                tombstones.insert(
                    (logical.logical_root.as_token(), logical.relative_path),
                    "observed deletion or rename".into(),
                );
            }
        }
        if let Some(previous_path) = &mutation.previous_path {
            if let Some(logical) = roots.classify(Path::new(previous_path)) {
                inherited_files.remove(&(
                    logical.logical_root.as_token(),
                    logical.relative_path.clone(),
                ));
                tombstones.insert(
                    (logical.logical_root.as_token(), logical.relative_path),
                    "observed deletion or rename".into(),
                );
            }
        }
    }

    let indexed_states = agent
        .db
        .list_file_states(app_id, None)?
        .into_iter()
        .map(|state| {
            (
                (state.logical_root.clone(), state.relative_path.clone()),
                state,
            )
        })
        .collect::<HashMap<_, _>>();
    let mut include_paths = Vec::new();
    let mut changed_files = BTreeMap::<String, (PathRecord, PathAssociation, LogicalPath)>::new();
    for (canonical, (record, association)) in candidates {
        let logical = roots.classify(Path::new(&canonical)).unwrap_or_else(|| {
            LogicalPath::new(
                LogicalRoot::Home,
                canonical.trim_start_matches('/').to_string(),
            )
        });
        let inherited = inherited_files.remove(&(
            logical.logical_root.as_token(),
            logical.relative_path.clone(),
        ));
        if !Path::new(&canonical).is_file() {
            if inherited.is_some() {
                tombstones.insert(
                    (logical.logical_root.as_token(), logical.relative_path),
                    "observed deletion or rename".into(),
                );
            }
            continue;
        }
        let decision = classifier.decide(&record, &association, mode)?;
        let complete_install_content = mode == BackupMode::CompleteApplication
            && install_roots
                .iter()
                .any(|root| Path::new(&canonical).starts_with(root));
        match decision {
            BackupDecision::Exclude => continue,
            BackupDecision::MetadataOnly | BackupDecision::DeferAndReconcile
                if !complete_install_content =>
            {
                continue;
            }
            _ => {}
        }
        // A delete followed by recreation in the same capture has live content.
        tombstones.remove(&(
            logical.logical_root.as_token(),
            logical.relative_path.clone(),
        ));

        let metadata = std::fs::metadata(&canonical)?;
        let mtime_ns = metadata_mtime_ns(&metadata).unwrap_or(0);
        #[cfg(unix)]
        let inode = Some(metadata.ino());
        #[cfg(not(unix))]
        let inode = None;
        let indexed = indexed_states.get(&(
            logical.logical_root.as_token(),
            logical.relative_path.clone(),
        ));
        if performance != BackupPerformanceMode::Full
            && !requires_reconciliation
            && !dirty_path_ids.contains(&record.path_id)
            && !mutated_paths.contains(canonical.as_str())
            && indexed.is_some_and(|state| {
                state.fast_identity_matches(metadata.len(), mtime_ns, inode, None)
                    && inherited
                        .as_ref()
                        .is_some_and(|file| state.content_hash.as_ref() == Some(&file.file_hash))
                    && state.mode == Some(metadata.mode())
            })
            && inherited.is_some()
        {
            manifest
                .files
                .push(inherited.expect("checked inherited file"));
            metrics.num_files_skipped_fast_identity =
                metrics.num_files_skipped_fast_identity.saturating_add(1);
            continue;
        }
        include_paths.push(PathBuf::from(&canonical));
        changed_files.insert(canonical, (record, association, logical));
    }
    let changed_path_ids = changed_files
        .values()
        .map(|(record, _, _)| record.path_id)
        .collect::<Vec<_>>();
    manifest.files.extend(inherited_files.into_values());
    let shared_app_ids_by_path = agent
        .db
        .associations_for_paths(&changed_path_ids)?
        .into_iter()
        .map(|(path_id, associations)| {
            (
                path_id,
                associations
                    .into_iter()
                    .filter(|other| {
                        other.app_id != *app_id && other.confidence >= OWNERSHIP_CANDIDATE_MIN
                    })
                    .map(|other| other.app_id)
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<HashMap<_, _>>();

    for ((logical_root, relative_path), reason) in tombstones {
        manifest.tombstones.push(ManifestTombstone {
            logical_root,
            relative_path,
            reason,
        });
    }
    metrics.planning_duration_ms = elapsed_ms(planning_started);
    let mut progress = OperationProgress::new("snapshotting", 0);
    progress.total_units = Some(include_paths.len() as u64);
    progress.detail_json = serde_json::json!({
        "performance_mode": performance.as_str(),
        "files_reused": metrics.num_files_skipped_fast_identity,
    });
    progress.unit = Some("files".into());
    agent.db.set_operation_progress(op_id, Some(&progress))?;

    if include_paths.is_empty()
        && manifest.tombstones.is_empty()
        && parent_scope == Some((mode, manifest.files.len()))
    {
        if let Some(parent_bundle_id) = manifest.parent_bundle_id {
            if let Ok(parent_manifest) =
                read_committed_manifest(provider, master, app_id, parent_bundle_id).await
            {
                metrics.total_duration_ms =
                    elapsed_ms(total_started).saturating_add(metrics.discovery_duration_ms);
                finish_backup_evidence(agent, app_id, &pending_mutations, captured_generation)?;
                mark_noop_completed(agent, &mut op, &mut metrics, &parent_manifest)?;
                return Ok(parent_manifest);
            }
        }
    }

    persist_operation(agent, &mut op, BackupState::Snapshotting, &metrics)?;
    let atomic_view =
        noland_snapshot::create_atomic_view(&agent.config.paths.snapshots, &include_paths)?;
    let atomic_cleanup = atomic_view
        .as_ref()
        .map(|view| SnapshotCleanup(view.root.clone()));
    let staged_paths = atomic_view
        .as_ref()
        .map(|view| {
            view.mappings
                .iter()
                .map(|mapping| (mapping.source.clone(), mapping.staged.clone()))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    // Snapshot only active hashing jobs. In copy-fallback mode an entire game
    // must not be duplicated before hashing starts.
    let sizes = include_paths
        .iter()
        .map(std::fs::metadata)
        .collect::<std::io::Result<Vec<_>>>()?;
    let bytes = sizes.iter().map(|meta| meta.len()).sum::<u64>();
    let mut snapshot_sizes = sizes.iter().map(|meta| meta.len()).collect::<Vec<_>>();
    snapshot_sizes.sort_unstable_by(|left, right| right.cmp(left));
    let snapshot_budget = if atomic_view.is_some() {
        0
    } else {
        snapshot_sizes
            .iter()
            .take(hashing_worker_count(performance))
            .sum()
    };
    let _disk_budget = disk::DiskReservation::reserve(&[
        (agent.config.paths.snapshots.clone(), snapshot_budget),
        (agent.config.paths.cache.clone(), bytes),
        (
            agent.config.paths.packs.clone(),
            bytes
                .saturating_add(bytes / 100)
                .saturating_add(1024 * 1024),
        ),
    ])?;
    manifest.consistency = ConsistencyKind::Snapshot;

    persist_operation(agent, &mut op, BackupState::Hashing, &metrics)?;
    progress.phase = "hashing".into();
    progress.completed_units = 0;
    agent.db.set_operation_progress(op_id, Some(&progress))?;
    let hashing_started = Instant::now();
    let cas = LocalCas::new(agent.config.paths.cache.join("cas/chunks"))?;
    let cas_lease = Arc::new(cas.lease()?);
    let cancelled: CancellationCheck = {
        let operations = agent.operations.clone();
        Arc::new(move || operations.cancel_requested(op_id))
    };
    let snapshot_time = Utc::now();
    let storage_id = repository_cache_binding(provider.storage_identity(), master);
    let remote_chunks = load_remote_chunk_index(agent, storage_id.as_deref(), snapshot_time)?;
    let cas_observed_at = snapshot_time;
    let mut cas_observations = Vec::<LocalCasEntry>::new();
    let mut progress_reporter = ProgressReporter::new(op_id);
    let inherited_hashes = pack_index
        .iter()
        .map(|entry| entry.chunk_hash.clone())
        .collect::<std::collections::HashSet<_>>();
    let jobs = changed_files
        .iter()
        .map(|(source, (record, association, logical))| HashJob {
            app_id: app_id.clone(),
            source: PathBuf::from(source),
            snapshot_root: agent.config.paths.snapshots.clone(),
            staged: staged_paths.get(&PathBuf::from(source)).cloned(),
            record: record.clone(),
            association: association.clone(),
            logical: logical.clone(),
            shared_app_ids: shared_app_ids_by_path
                .get(&record.path_id)
                .cloned()
                .unwrap_or_default(),
        })
        .collect::<Vec<_>>();
    let mut new_chunk_paths = BTreeMap::<String, PathBuf>::new();
    let mut small_file_chunk_hashes = BTreeSet::<String>::new();
    let mut trusted_states = Vec::<FileStateRecord>::new();
    hash_files_parallel(
        jobs,
        hashing_worker_count(performance),
        Arc::clone(&cas_lease),
        inherited_hashes,
        remote_chunks,
        cas_observed_at,
        Arc::clone(&cancelled),
        |result| {
            metrics.snapshot_duration_ms = metrics
                .snapshot_duration_ms
                .saturating_add(result.snapshot_duration_ms);
            if result.consistency == ConsistencyKind::BestEffort {
                manifest.consistency = ConsistencyKind::BestEffort;
            }
            metrics.num_files_rehashed = metrics.num_files_rehashed.saturating_add(1);
            metrics.bytes_scanned = metrics
                .bytes_scanned
                .saturating_add(result.metrics.bytes_hashed);
            metrics.bytes_hashed = metrics
                .bytes_hashed
                .saturating_add(result.metrics.bytes_hashed);
            metrics.bytes_chunked = metrics
                .bytes_chunked
                .saturating_add(result.metrics.bytes_hashed);
            metrics.num_chunks_reused = metrics
                .num_chunks_reused
                .saturating_add(result.metrics.chunks_reused);
            metrics.num_chunks_created = metrics
                .num_chunks_created
                .saturating_add(result.metrics.chunks_created);
            metrics.num_local_cas_hits = metrics
                .num_local_cas_hits
                .saturating_add(result.metrics.local_cas_hits);
            metrics.num_remote_index_hits = metrics
                .num_remote_index_hits
                .saturating_add(result.metrics.remote_index_hits);
            metrics.bytes_reused_local = metrics
                .bytes_reused_local
                .saturating_add(result.metrics.bytes_reused_local);
            pack_index.extend(result.remote_entries);
            new_chunk_paths.extend(result.new_chunks);
            cas_observations.extend(result.cas_observations);
            small_file_chunk_hashes.extend(result.small_file_chunk_hashes);
            trusted_states.push(result.file_state);
            manifest.files.push(result.manifest_file);
            progress_reporter.record_file(result.metrics.bytes_hashed);
            progress_reporter.maybe_flush(agent, &mut progress, &metrics)
        },
    )?;
    progress_reporter.flush(agent, &mut progress, &metrics)?;
    drop(atomic_cleanup);
    metrics.hashing_duration_ms = elapsed_ms(hashing_started);

    persist_operation(agent, &mut op, BackupState::Packing, &metrics)?;
    progress.phase = "packing".into();
    agent.db.set_operation_progress(op_id, Some(&progress))?;
    let packing_started = Instant::now();
    let mut small_chunks = Vec::new();
    let mut regular_chunks = Vec::new();
    let mut ordered_files = manifest.files.iter().collect::<Vec<_>>();
    ordered_files.sort_by(|left, right| {
        noland_restore::restore_priority(left)
            .cmp(&noland_restore::restore_priority(right))
            .then_with(|| {
                (&left.logical_root, &left.relative_path)
                    .cmp(&(&right.logical_root, &right.relative_path))
            })
    });
    for file in ordered_files {
        for chunk in &file.chunks {
            if let Some(path) = new_chunk_paths.remove(&chunk.hash) {
                let entry = (chunk.hash.clone(), path);
                if small_file_chunk_hashes.contains(&chunk.hash) {
                    small_chunks.push(entry);
                } else {
                    regular_chunks.push(entry);
                }
            }
        }
    }
    // Small state files are deliberately grouped into compact packs so launch-critical
    // restore does not need to fetch a mostly unrelated 512 MiB pack.
    let plans = packing::plans(small_chunks, regular_chunks)?;
    pack_index.sort_by(|left, right| left.chunk_hash.cmp(&right.chunk_hash));
    pack_index.dedup_by(|left, right| left.chunk_hash == right.chunk_hash);
    manifest.files.sort_by(|left, right| {
        (&left.logical_root, &left.relative_path).cmp(&(&right.logical_root, &right.relative_path))
    });
    let referenced_chunks = manifest
        .files
        .iter()
        .flat_map(|file| &file.chunks)
        .map(|chunk| chunk.hash.as_str())
        .collect::<std::collections::HashSet<_>>();
    pack_index.retain(|entry| referenced_chunks.contains(entry.chunk_hash.as_str()));
    manifest.tombstones.sort_by(|left, right| {
        (&left.logical_root, &left.relative_path).cmp(&(&right.logical_root, &right.relative_path))
    });
    if mode == BackupMode::CompleteApplication && manifest.files.is_empty() {
        return Err(StateError::Invalid(format!(
            "complete application backup for {app_id} contains no application files; \
             the application is not installed on this instance"
        )));
    }
    noland_restore::embed_restore_plan(&mut manifest, restore_mode_for_backup(mode));
    metrics.packing_duration_ms = elapsed_ms(packing_started);

    let prepared = PreparedBackup {
        version: 1,
        storage_id: provider
            .storage_identity()
            .map(|identity| identity.cache_key()),
        manifest,
        pack_index,
        pack_files: Vec::new(),
        new_pack_entries: Vec::new(),
        cas_observations,
        trusted_states,
        pending_mutations,
        captured_generation,
        incremental: 0,
        metrics,
        checksums: BTreeMap::new(),
        release_lease: None,
        packing: Some(packing::PackingState {
            plans,
            lease: cas_lease.root().to_path_buf(),
            workers: pack_worker_count(performance),
        }),
    };
    // Preserve immutable IDs and ciphertext across retries. Upload journal rows
    // now remain meaningful; a retry resumes this captured revision, not new data.
    agent.db.delete_sync_journal_entries_for_kind_direction(
        op_id,
        ContentObjectKind::Pack,
        SyncDirection::Upload,
    )?;
    cas_lease.sync_all()?;
    write_prepared_backup(agent, master, op_id, &prepared)?;
    if prepared.storage_id.is_some() {
        cas_lease.retain_for_retry();
    }
    drop(_disk_budget);
    finish_prepared_backup(agent, provider, master, op, prepared, total_started, false).await
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PreparedBackup {
    version: u32,
    storage_id: Option<String>,
    manifest: BundleManifest,
    pack_index: Vec<PackIndexEntry>,
    pack_files: Vec<(String, PathBuf)>,
    new_pack_entries: Vec<PackIndexEntry>,
    cas_observations: Vec<LocalCasEntry>,
    trusted_states: Vec<FileStateRecord>,
    pending_mutations: Vec<AppMutationRecord>,
    captured_generation: i64,
    incremental: u64,
    metrics: OperationMetrics,
    checksums: BTreeMap<String, String>,
    packing: Option<packing::PackingState>,
    // Retain cleanup ownership across a crash after the sealed checkpoint is
    // durable but before its plaintext lease has been released.
    #[serde(default)]
    release_lease: Option<PathBuf>,
}

fn prepared_backup_path(agent: &StateAgent, operation_id: Uuid) -> PathBuf {
    agent
        .config
        .paths
        .packs
        .join("resume")
        .join(format!("{operation_id}.enc"))
}

fn write_prepared_backup(
    agent: &StateAgent,
    master: &MasterKey,
    operation_id: Uuid,
    prepared: &PreparedBackup,
) -> Result<()> {
    use std::io::Write;
    if prepared.storage_id.is_none() {
        return Ok(());
    }
    let path = prepared_backup_path(agent, operation_id);
    let parent = path.parent().expect("resume parent");
    std::fs::create_dir_all(parent)?;
    let encrypted = noland_crypto::wrap_envelope(
        &noland_crypto::derive_keys(master).manifest,
        format!("backup-resume:{operation_id}").as_bytes(),
        &serde_json::to_vec(prepared)?,
    )?;
    let temp = path.with_extension("partial");
    disk::ensure_space(parent, encrypted.len() as u64)?;
    let mut file = std::fs::File::create(&temp)?;
    file.write_all(&encrypted)?;
    file.sync_all()?;
    std::fs::rename(temp, &path)?;
    std::fs::File::open(parent)?.sync_all()?;
    if let Some(parent) = parent.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn read_prepared_backup(
    agent: &StateAgent,
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    operation_id: Uuid,
    app_id: &AppId,
    mode: BackupMode,
) -> Result<Option<PreparedBackup>> {
    let path = prepared_backup_path(agent, operation_id);
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let plain = noland_crypto::unwrap_envelope(
        &noland_crypto::derive_keys(master).manifest,
        format!("backup-resume:{operation_id}").as_bytes(),
        &bytes,
    )?;
    let prepared: PreparedBackup = serde_json::from_slice(&plain)?;
    let storage_id = provider
        .storage_identity()
        .map(|identity| identity.cache_key());
    if prepared.version != 1
        || storage_id.is_none()
        || prepared.storage_id != storage_id
        || prepared.manifest.app.app_id != *app_id
        || prepared.manifest.mode != mode
    {
        return Err(StateError::Invalid(
            "backup retry does not match its captured app, mode or repository".into(),
        ));
    }
    for (id, path) in &prepared.pack_files {
        let expected = agent
            .config
            .paths
            .packs
            .join(prepared.manifest.bundle_id.to_string())
            .join(format!("{id}.pack"));
        if Uuid::parse_str(id).is_err() || path != &expected {
            return Err(StateError::UnsafePath("invalid resumed pack path".into()));
        }
        if agent
            .db
            .sync_journal_completed(operation_id, &pack_key(id))?
        {
            continue;
        }
        if std::fs::symlink_metadata(path)?.file_type().is_symlink()
            || prepared.checksums.get(id) != Some(&noland_cas::blake3_file(path)?)
        {
            return Err(StateError::Integrity("resumed pack changed on disk".into()));
        }
    }
    Ok(Some(prepared))
}

async fn finish_prepared_backup(
    agent: &StateAgent,
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    mut op: OperationRecord,
    mut prepared: PreparedBackup,
    total_started: Instant,
    resumed: bool,
) -> Result<BundleManifest> {
    if agent.operations.cancel_requested(op.operation_id) {
        return Err(cancellation_error());
    }
    let storage_before = provider.operation_metrics();
    packing::pack_and_upload(
        agent,
        provider,
        master,
        op.operation_id,
        &mut prepared,
        resumed,
    )
    .await?;
    if agent.operations.cancel_requested(op.operation_id) {
        return Err(cancellation_error());
    }
    let PreparedBackup {
        manifest,
        pack_index,
        pack_files,
        new_pack_entries,
        cas_observations,
        mut trusted_states,
        pending_mutations,
        captured_generation,
        incremental,
        mut metrics,
        ..
    } = prepared;
    let app_id = &manifest.app.app_id;
    let op_id = op.operation_id;
    let mut pack_cleanup = PackCleanup::new(
        agent
            .config
            .paths
            .packs
            .join(manifest.bundle_id.to_string()),
    );
    let mut progress = OperationProgress::new("uploading", 0);

    let transfer_bytes = pack_files
        .iter()
        .filter_map(|(_, path)| std::fs::metadata(path).ok().map(|metadata| metadata.len()))
        .sum::<u64>();
    persist_operation(agent, &mut op, BackupState::Uploading, &metrics)?;
    progress.phase = "uploading".into();
    progress.completed_units = 0;
    progress.total_units = Some(transfer_bytes);
    progress.unit = Some("bytes".into());
    progress.message = Some(format!(
        "Uploading {transfer_bytes} bytes in encrypted packs"
    ));
    progress.detail_json = serde_json::json!({
        "bytes_to_upload": transfer_bytes,
        "total_packs": pack_files.len(),
        "completed_pack_bytes": 0,
        "bytes_transferred": 0,
        "total_transfer_bytes": transfer_bytes,
    });
    progress.updated_at = Utc::now();
    progress.detail_json["resumed"] = serde_json::json!(resumed);
    agent.db.set_operation_progress(op_id, Some(&progress))?;
    let upload_started = Instant::now();
    agent.db.record_commit(
        manifest.commit_id,
        app_id,
        manifest.bundle_id,
        "pending",
        None,
        CommitVisibility::Uploading,
    )?;
    let index_json = serde_json::to_vec(&pack_index)?;
    let committed_key = noland_storage::RemoteKey::new(format!(
        "{}/{}",
        bundle_dir(app_id, manifest.bundle_id),
        COMMITTED_MARKER
    ));
    let already_committed = if resumed && provider.stat(&committed_key).await?.is_some() {
        let existing =
            read_committed_manifest(provider, master, app_id, manifest.bundle_id).await?;
        let existing_index = read_pack_index(provider, master, app_id, manifest.bundle_id).await?;
        if serde_json::to_vec(&existing)? != serde_json::to_vec(&manifest)?
            || serde_json::to_vec(&existing_index)? != index_json
        {
            return Err(StateError::Integrity(
                "committed revision differs from retry checkpoint".into(),
            ));
        }
        let hash = format!(
            "blake3:{}",
            blake3::hash(&serde_json::to_vec(&manifest)?).to_hex()
        );
        agent.db.record_commit(
            manifest.commit_id,
            app_id,
            manifest.bundle_id,
            &hash,
            Some(&bundle_dir(app_id, manifest.bundle_id)),
            CommitVisibility::Committed,
        )?;
        true
    } else {
        false
    };
    if !already_committed {
        commit_bundle_with_index_for_operation(
            provider,
            master,
            &manifest,
            &pack_files,
            Some(&index_json),
            Some(&agent.db),
            Some(op_id),
        )
        .await?;
    }
    metrics.upload_duration_ms = metrics
        .upload_duration_ms
        .saturating_add(elapsed_ms(upload_started));
    metrics.num_manifest_writes = 1;

    persist_operation(agent, &mut op, BackupState::Committing, &metrics)?;
    let commit_started = Instant::now();
    update_catalog_with_bundle(provider, master, &manifest, incremental).await?;
    let cache_binding = repository_cache_binding(provider.storage_identity(), master);
    cache_parent(agent, &manifest, &pack_index, cache_binding.as_deref())?;
    metrics.commit_duration_ms = elapsed_ms(commit_started);
    let storage_metrics = provider.operation_metrics().saturating_sub(storage_before);
    apply_storage_metrics(&mut metrics, storage_metrics);

    let chunk_rows = new_pack_entries
        .iter()
        .map(|entry| {
            (
                entry.chunk_hash.clone(),
                Some(entry.pack_id.clone()),
                u64::from(entry.plaintext_len),
            )
        })
        .collect::<Vec<_>>();
    agent.db.remember_chunks_bulk(&chunk_rows)?;
    remember_remote_pack_index(agent, provider.storage_identity(), master, &pack_index)?;
    agent.db.upsert_local_cas_entries(
        &cas_observations
            .into_iter()
            .filter(|entry| Path::new(&entry.local_path).is_file())
            .collect::<Vec<_>>(),
    )?;
    if resumed {
        for state in &mut trusted_states {
            state.trust = FileStateTrust::VerifyRequired;
        }
    }
    agent.db.upsert_file_states(&trusted_states)?;
    finish_backup_evidence(agent, app_id, &pending_mutations, captured_generation)?;
    if resumed {
        agent.db.mark_dirty(app_id, None, true)?;
    }

    persist_operation(agent, &mut op, BackupState::Checkpointing, &metrics)?;
    let checkpoint_started = Instant::now();
    let checkpoint_error = crate::checkpoint::write_local_checkpoint(agent)
        .err()
        .map(|error| error.to_string());
    if let Some(error) = checkpoint_error.as_ref() {
        tracing::error!(
            operation_id = %op_id,
            %error,
            "local checkpoint after backup commit failed"
        );
    }
    metrics.checkpoint_duration_ms = elapsed_ms(checkpoint_started);

    metrics.total_duration_ms =
        elapsed_ms(total_started).saturating_add(metrics.discovery_duration_ms);
    let detail = op
        .detail_json
        .as_object_mut()
        .expect("operation detail is always an object");
    detail.insert("bundle_id".into(), serde_json::json!(manifest.bundle_id));
    detail.insert("commit_id".into(), serde_json::json!(manifest.commit_id));
    if let Some(error) = checkpoint_error {
        detail.insert("checkpoint_error".into(), serde_json::json!(error));
    }
    agent.db.set_operation_progress(op_id, None)?;
    persist_operation(agent, &mut op, BackupState::Completed, &metrics)?;
    pack_cleanup.mark_cloud_committed();
    match std::fs::remove_file(prepared_backup_path(agent, op_id)) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(%error, "could not remove completed backup resume metadata"),
    }
    Ok(manifest)
}

async fn load_parent(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    agent: &StateAgent,
    app_id: &AppId,
) -> Option<(BundleManifest, Vec<PackIndexEntry>)> {
    let (_, bundle_id, _) = agent.db.latest_commit(app_id).ok().flatten()?;
    let binding = repository_cache_binding(provider.storage_identity(), master);
    if let Some(cached) = read_cached_parent(agent, app_id, bundle_id, binding.as_deref()) {
        let _ = remember_remote_pack_index(agent, provider.storage_identity(), master, &cached.1);
        return Some(cached);
    }
    let manifest = read_committed_manifest(provider, master, app_id, bundle_id)
        .await
        .ok()?;
    let index = read_pack_index(provider, master, app_id, bundle_id)
        .await
        .ok()?;
    cache_parent(agent, &manifest, &index, binding.as_deref()).ok()?;
    let _ = remember_remote_pack_index(agent, provider.storage_identity(), master, &index);
    Some((manifest, index))
}

fn repository_cache_binding(
    identity: Option<ProviderRootIdentity>,
    master: &MasterKey,
) -> Option<String> {
    let identity = identity?;
    // Opaque, domain-separated cache scope; neither the key nor credentials are
    // persisted. Reuse must match both the remote namespace and the vault key.
    Some(
        blake3::keyed_hash(
            &master.0,
            format!("noland-cache-v1:{}", identity.cache_key()).as_bytes(),
        )
        .to_hex()
        .to_string(),
    )
}

fn parent_cache_dir(agent: &StateAgent, app_id: &AppId, bundle_id: Uuid) -> PathBuf {
    let app_key = blake3::hash(app_id.as_str().as_bytes())
        .to_hex()
        .to_string();
    agent
        .config
        .paths
        .cache
        .join("manifests")
        .join(app_key)
        .join(bundle_id.to_string())
}

fn read_cached_parent(
    agent: &StateAgent,
    app_id: &AppId,
    bundle_id: Uuid,
    binding: Option<&str>,
) -> Option<(BundleManifest, Vec<PackIndexEntry>)> {
    let dir = parent_cache_dir(agent, app_id, bundle_id).join(binding?);
    let manifest: BundleManifest =
        serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).ok()?).ok()?;
    if manifest.bundle_id != bundle_id || manifest.app.app_id != *app_id {
        return None;
    }
    let index = serde_json::from_slice(&std::fs::read(dir.join("index.json")).ok()?).ok()?;
    Some((manifest, index))
}

fn load_remote_chunk_index(
    agent: &StateAgent,
    storage_id: Option<&str>,
    observed_at: chrono::DateTime<Utc>,
) -> Result<RemoteChunkIndex> {
    let Some(storage_id) = storage_id else {
        return Ok(RemoteChunkIndex::empty());
    };
    let mut entries = HashMap::new();
    for entry in agent
        .db
        .list_remote_content_entries(storage_id, ContentObjectKind::Chunk)?
    {
        if entry.state != RemoteContentState::Present || !entry.is_fresh_at(observed_at) {
            continue;
        }
        let Some(payload) = entry.etag else {
            continue;
        };
        if let Ok(pack_entry) = serde_json::from_str::<PackIndexEntry>(&payload) {
            entries.insert(entry.content_hash, pack_entry);
        }
    }
    Ok(RemoteChunkIndex { entries })
}

pub(crate) fn remember_remote_pack_index(
    agent: &StateAgent,
    identity: Option<ProviderRootIdentity>,
    master: &MasterKey,
    index: &[PackIndexEntry],
) -> Result<()> {
    let Some(storage_id) = repository_cache_binding(identity, master) else {
        return Ok(());
    };
    let now = Utc::now();
    let entries = index
        .iter()
        .map(|entry| {
            Ok(RemoteContentEntry {
                storage_id: storage_id.clone(),
                object_kind: ContentObjectKind::Chunk,
                content_hash: entry.chunk_hash.clone(),
                remote_path: noland_state_core::pack_key(&entry.pack_id),
                size: Some(u64::from(entry.plaintext_len)),
                etag: Some(serde_json::to_string(entry)?),
                state: RemoteContentState::Present,
                observed_at: now,
                expires_at: None,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    agent.db.upsert_remote_content_entries(&entries)
}

fn cache_parent(
    agent: &StateAgent,
    manifest: &BundleManifest,
    index: &[PackIndexEntry],
    binding: Option<&str>,
) -> Result<()> {
    let Some(binding) = binding else {
        return Ok(());
    };
    let dir = parent_cache_dir(agent, &manifest.app.app_id, manifest.bundle_id).join(binding);
    std::fs::create_dir_all(&dir)?;
    let manifest_temp = dir.join("manifest.json.tmp");
    let index_temp = dir.join("index.json.tmp");
    std::fs::write(&manifest_temp, serde_json::to_vec(manifest)?)?;
    std::fs::write(&index_temp, serde_json::to_vec(index)?)?;
    std::fs::rename(manifest_temp, dir.join("manifest.json"))?;
    std::fs::rename(index_temp, dir.join("index.json"))?;
    Ok(())
}

fn manifest_source_path(file: &ManifestFile, roots: &LogicalRootMap) -> Option<PathBuf> {
    file.source_path_hint
        .as_ref()
        .map(PathBuf::from)
        .or_else(|| {
            let root = file.logical_root_parsed()?;
            roots
                .resolve(&root)
                .map(|base| base.join(&file.relative_path))
        })
}

fn file_state_from_manifest(
    app_id: &AppId,
    file: &ManifestFile,
    record: &PathRecord,
) -> FileStateRecord {
    FileStateRecord {
        app_id: app_id.clone(),
        logical_root: file.logical_root.clone(),
        relative_path: file.relative_path.clone(),
        canonical_path: file.source_path_hint.clone(),
        file_type: match file.file_type.as_str() {
            "directory" => FileType::Directory,
            "symlink" => FileType::Symlink,
            "other" => FileType::Other,
            _ => FileType::File,
        },
        size: file.size,
        mtime_ns: file.mtime_ns.unwrap_or(0),
        inode: record.inode.and_then(|value| u64::try_from(value).ok()),
        mount_id: record.mount_id.and_then(|value| u64::try_from(value).ok()),
        mode: file.mode,
        content_hash: Some(file.file_hash.clone()),
        trust: FileStateTrust::Trusted,
        last_seen_at: Utc::now(),
        last_hashed_at: Some(Utc::now()),
    }
}

fn finish_backup_evidence(
    agent: &StateAgent,
    app_id: &AppId,
    mutations: &[AppMutationRecord],
    captured_generation: i64,
) -> Result<()> {
    agent
        .db
        .finish_backup_mutations(app_id, mutations, captured_generation)
}

fn mark_noop_completed(
    agent: &StateAgent,
    op: &mut OperationRecord,
    metrics: &mut OperationMetrics,
    manifest: &BundleManifest,
) -> Result<()> {
    let detail = op
        .detail_json
        .as_object_mut()
        .expect("operation detail is always an object");
    detail.insert("bundle_id".into(), serde_json::json!(manifest.bundle_id));
    detail.insert("commit_id".into(), serde_json::json!(manifest.commit_id));
    detail.insert("no_op".into(), serde_json::json!(true));
    agent.db.set_operation_progress(op.operation_id, None)?;
    persist_operation(agent, op, BackupState::Completed, metrics)
}

fn apply_storage_metrics(
    metrics: &mut OperationMetrics,
    storage_metrics: noland_storage::StorageOperationMetrics,
) {
    metrics.bytes_uploaded = metrics
        .bytes_uploaded
        .saturating_add(storage_metrics.bytes_uploaded);
    metrics.bytes_downloaded = metrics
        .bytes_downloaded
        .saturating_add(storage_metrics.bytes_downloaded);
    metrics.num_rclone_invocations = metrics
        .num_rclone_invocations
        .saturating_add(storage_metrics.rclone_invocations);
    metrics.num_remote_stat_calls = metrics
        .num_remote_stat_calls
        .saturating_add(storage_metrics.remote_stat_calls);
    metrics.num_remote_list_calls = metrics
        .num_remote_list_calls
        .saturating_add(storage_metrics.remote_list_calls);
    metrics.num_remote_mkdir_calls = metrics
        .num_remote_mkdir_calls
        .saturating_add(storage_metrics.remote_mkdir_calls);
    metrics.num_remote_upload_calls = metrics
        .num_remote_upload_calls
        .saturating_add(storage_metrics.remote_upload_calls);
    metrics.num_remote_download_calls = metrics
        .num_remote_download_calls
        .saturating_add(storage_metrics.remote_download_calls);
}

fn metadata_mtime_ns(metadata: &std::fs::Metadata) -> Option<i64> {
    metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
}

fn track_steam_appmanifest(
    agent: &StateAgent,
    identity: &AppIdentity,
    roots: &LogicalRootMap,
    install_roots: &[PathBuf],
) -> Result<Option<(PathRecord, PathAssociation)>> {
    let Some(steam_app_id) = identity.steam_app_id.or_else(|| {
        identity
            .app_id
            .as_str()
            .strip_prefix("steam:")
            .and_then(|value| value.parse::<u32>().ok())
    }) else {
        return Ok(None);
    };
    let Some(path) = steam_appmanifest_path(steam_app_id, roots, install_roots) else {
        return Ok(None);
    };
    let metadata = std::fs::metadata(&path)?;
    let canonical_path = path.to_string_lossy().into_owned();
    let logical = roots
        .classify(&path)
        .ok_or_else(|| StateError::NotFound(format!("logical root for {}", path.display())))?;
    let path_id = agent.db.upsert_path(&canonical_path)?;
    let now = Utc::now();

    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;

    let record = PathRecord {
        path_id,
        canonical_path,
        logical_root: Some(logical.logical_root.as_token()),
        relative_path: Some(logical.relative_path),
        file_type: Some("file".into()),
        #[cfg(unix)]
        inode: Some(metadata.ino() as i64),
        #[cfg(not(unix))]
        inode: None,
        mount_id: None,
        size: Some(metadata.len() as i64),
        mtime_ns: metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|value| value.as_nanos().min(i64::MAX as u128) as i64),
        #[cfg(unix)]
        mode: Some(metadata.mode() as i64),
        #[cfg(not(unix))]
        mode: None,
        #[cfg(unix)]
        uid: Some(metadata.uid() as i64),
        #[cfg(not(unix))]
        uid: None,
        #[cfg(unix)]
        gid: Some(metadata.gid() as i64),
        #[cfg(not(unix))]
        gid: None,
        content_hash: None,
        last_scanned_at: Some(now.timestamp()),
    };
    agent.db.update_path_meta(path_id, &record)?;

    let association = PathAssociation {
        app_id: identity.app_id.clone(),
        path_id,
        confidence: CONF_EXPLICIT,
        evidence: vec![Evidence::new(EvidenceKind::SteamMetadata)],
        persistence_class: PersistenceClass::PersistentState,
        semantic_role: SemanticRole::AppContent,
        first_seen_at: now,
        last_seen_at: now,
    };
    agent.db.upsert_association(&association)?;

    Ok(Some((record, association)))
}

pub async fn run_backup_to_local(
    agent: &StateAgent,
    app_id: &AppId,
    mode: BackupMode,
    cloud_root: PathBuf,
    master: &MasterKey,
) -> Result<BundleManifest> {
    let storage = LocalStorage::new(cloud_root);
    storage.ensure_root().await?;
    run_backup(
        agent,
        app_id,
        mode,
        BackupPerformanceMode::Balanced,
        &storage,
        master,
        None,
    )
    .await
}

/// Backup using an ephemeral rclone session minted by the desktop adapter.
pub async fn run_backup_with_session(
    agent: &StateAgent,
    app_id: &AppId,
    mode: BackupMode,
    session: &EphemeralRcloneSession,
    master: &MasterKey,
    operation_id: Option<Uuid>,
) -> Result<BundleManifest> {
    run_backup_with_session_performance(
        agent,
        app_id,
        mode,
        BackupPerformanceMode::Balanced,
        session,
        master,
        operation_id,
    )
    .await
}

pub async fn run_backup_with_session_performance(
    agent: &StateAgent,
    app_id: &AppId,
    mode: BackupMode,
    performance: BackupPerformanceMode,
    session: &EphemeralRcloneSession,
    master: &MasterKey,
    operation_id: Option<Uuid>,
) -> Result<BundleManifest> {
    let (config_path, _session_guard) =
        write_guarded_ephemeral_session(&agent.config.paths.run_root, session)?;
    let tuning = transfer_tuning(agent, performance, session);
    let storage =
        RcloneStorage::try_from_session(session, &config_path)?.with_transfer_tuning(tuning);
    run_backup(
        agent,
        app_id,
        mode,
        performance,
        &storage,
        master,
        operation_id,
    )
    .await
}

pub async fn run_backup_all_with_session(
    agent: &StateAgent,
    mode: BackupMode,
    session: &EphemeralRcloneSession,
    master: &MasterKey,
) -> Result<Vec<BundleManifest>> {
    run_backup_all_with_session_performance(
        agent,
        mode,
        BackupPerformanceMode::Balanced,
        session,
        master,
        None,
    )
    .await
}

pub async fn run_backup_all_with_session_performance(
    agent: &StateAgent,
    mode: BackupMode,
    performance: BackupPerformanceMode,
    session: &EphemeralRcloneSession,
    master: &MasterKey,
    operation_id: Option<Uuid>,
) -> Result<Vec<BundleManifest>> {
    let (config_path, _session_guard) =
        write_guarded_ephemeral_session(&agent.config.paths.run_root, session)?;
    let tuning = transfer_tuning(agent, performance, session);
    let storage =
        RcloneStorage::try_from_session(session, &config_path)?.with_transfer_tuning(tuning);
    run_backup_all(agent, mode, performance, &storage, master, operation_id).await
}

async fn run_backup_all(
    agent: &StateAgent,
    mode: BackupMode,
    performance: BackupPerformanceMode,
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    operation_id: Option<Uuid>,
) -> Result<Vec<BundleManifest>> {
    let dirty = agent.db.list_dirty_apps()?;
    let portable: std::collections::HashSet<String> =
        noland_discovery::filter_backup_candidates(agent.db.list_apps()?)
            .into_iter()
            .map(|a| a.app_id.as_str().to_string())
            .collect();
    let mut targets: Vec<AppId> = dirty
        .into_iter()
        .map(|d| d.app_id)
        .filter(|id| portable.contains(id.as_str()))
        .collect();
    if targets.is_empty() {
        targets = portable.into_iter().map(AppId).collect();
    }
    let mut out: Vec<BundleManifest> = Vec::new();
    provider.ensure_root().await?;
    let total = targets.len() as u64;
    for (index, app_id) in targets.iter().enumerate() {
        if let Some(operation_id) = operation_id {
            let mut progress = OperationProgress::new("backing_up_apps", index as u64);
            progress.total_units = Some(total);
            progress.unit = Some("apps".into());
            progress.message = Some(format!(
                "Backing up {} ({}/{})",
                app_id.as_str(),
                index + 1,
                total
            ));
            let completed_apps = out
                .iter()
                .map(|manifest| manifest.app.app_id.clone())
                .collect::<Vec<_>>();
            progress.detail_json = serde_json::json!({
                "app_id": app_id,
                "app_index": index,
                "app_count": total,
                "completed_apps": completed_apps,
            });
            agent
                .db
                .set_operation_progress(operation_id, Some(&progress))?;
        }
        match run_backup(agent, app_id, mode, performance, provider, master, None).await {
            Ok(manifest) => out.push(manifest),
            Err(StateError::NotFound(_)) => continue,
            Err(err) => return Err(err),
        }
    }
    Ok(out)
}

fn restore_mode_for_backup(mode: BackupMode) -> RestoreMode {
    match mode {
        BackupMode::PersonalState => RestoreMode::PersonalState,
        BackupMode::CompleteApplication => RestoreMode::CompleteApplication,
        BackupMode::Custom => RestoreMode::Custom,
    }
}

fn transfer_tuning(
    agent: &StateAgent,
    performance: BackupPerformanceMode,
    session: &EphemeralRcloneSession,
) -> TransferTuning {
    let provider = ProviderKind::parse(&session.provider).unwrap_or(ProviderKind::Local);
    if agent
        .db
        .open_session_app_ids()
        .is_ok_and(|sessions| !sessions.is_empty())
    {
        let tuning = TransferTuning::automatic(provider, TransferWorkload::GameplaySafe);
        log_transfer_tuning(provider, TransferWorkload::GameplaySafe, &tuning);
        return tuning;
    }
    let workload = match performance {
        BackupPerformanceMode::Fast | BackupPerformanceMode::Full => TransferWorkload::Throughput,
        BackupPerformanceMode::Balanced => TransferWorkload::Balanced,
    };
    let tuning = TransferTuning::automatic(provider, workload);
    log_transfer_tuning(provider, workload, &tuning);
    tuning
}

fn log_transfer_tuning(
    provider: ProviderKind,
    workload: TransferWorkload,
    tuning: &TransferTuning,
) {
    tracing::info!(
        provider = provider.as_str(),
        ?workload,
        uploads = tuning.max_parallel_uploads,
        downloads = tuning.max_parallel_downloads,
        rclone_transfers = tuning.rclone_transfers,
        rclone_checkers = tuning.rclone_checkers,
        "resolved automatic shared-storage parallelism"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentConfig;
    use noland_storage::{read_pack_index, LocalStorage};

    #[tokio::test]
    async fn complete_backup_skips_unchanged_content_and_revalidates_observer_gaps() {
        let root = test_root("complete-incremental");
        let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
        let app_id = AppId::desktop("complete-incremental");
        agent
            .db
            .upsert_app(&AppIdentity::new(app_id.clone(), "Fixture"))
            .unwrap();
        let mut saved = None;
        for index in 0..64 {
            saved = Some(tracked_save(
                &agent,
                &app_id,
                &format!("slot-{index}.sav"),
                format!("save-{index}").as_bytes(),
            ));
        }
        let master = MasterKey::generate();
        let storage = LocalStorage::new(root.join("cloud"));
        let first = run_backup(
            &agent,
            &app_id,
            BackupMode::CompleteApplication,
            BackupPerformanceMode::Balanced,
            &storage,
            &master,
            None,
        )
        .await
        .unwrap();
        let second = run_backup(
            &agent,
            &app_id,
            BackupMode::CompleteApplication,
            BackupPerformanceMode::Balanced,
            &storage,
            &master,
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            first.bundle_id, second.bundle_id,
            "an unchanged full capture is a no-op"
        );
        let (path, id) = saved.unwrap();
        std::fs::write(&path, b"new save with different bytes").unwrap();
        mark_mutated(&agent, &app_id, &path, id, AppMutationKind::Modify);
        let third = run_backup(
            &agent,
            &app_id,
            BackupMode::CompleteApplication,
            BackupPerformanceMode::Balanced,
            &storage,
            &master,
            None,
        )
        .await
        .unwrap();
        assert_eq!(third.files.len(), first.files.len());
        let op = agent
            .db
            .recent_operations(10)
            .unwrap()
            .into_iter()
            .find(|op| op.detail_json["bundle_id"] == serde_json::json!(third.bundle_id))
            .unwrap();
        assert_eq!(operation_metrics(&op).num_files_rehashed, 1);
        agent.db.mark_dirty(&app_id, None, true).unwrap();
        let operation_id = Uuid::new_v4();
        run_backup(
            &agent,
            &app_id,
            BackupMode::CompleteApplication,
            BackupPerformanceMode::Balanced,
            &storage,
            &master,
            Some(operation_id),
        )
        .await
        .unwrap();
        assert_eq!(
            operation_metrics(&agent.db.get_operation(operation_id).unwrap().unwrap())
                .num_files_rehashed,
            64
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pack_planning_preserves_input_file_chunk_order() {
        let root = test_root("pack-order");
        std::fs::create_dir_all(&root).unwrap();
        let chunks = ["z", "a", "m"]
            .into_iter()
            .map(|name| {
                let path = root.join(name);
                std::fs::write(&path, b"chunk").unwrap();
                (name.to_string(), path)
            })
            .collect();
        let plans = plan_pack_chunks(chunks, 100, 200).unwrap();
        assert_eq!(
            plans
                .into_iter()
                .flatten()
                .map(|entry| entry.0)
                .collect::<Vec<_>>(),
            ["z", "a", "m"]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    struct InterruptedUpload {
        inner: LocalStorage,
        fail: std::sync::atomic::AtomicBool,
        fail_pack: std::sync::atomic::AtomicBool,
        fail_catalog: std::sync::atomic::AtomicBool,
        uploads: std::sync::atomic::AtomicUsize,
        after_upload: Option<Box<dyn Fn() -> Result<()> + Send + Sync>>,
    }

    #[async_trait::async_trait]
    impl SharedStorageProvider for InterruptedUpload {
        async fn health_check(&self) -> Result<noland_storage::Health> {
            self.inner.health_check().await
        }
        async fn ensure_root(&self) -> Result<()> {
            self.inner.ensure_root().await
        }
        async fn stat(
            &self,
            key: &noland_storage::RemoteKey,
        ) -> Result<Option<noland_storage::RemoteMeta>> {
            self.inner.stat(key).await
        }
        async fn upload_immutable(
            &self,
            local: &Path,
            key: &noland_storage::RemoteKey,
        ) -> Result<noland_storage::RemoteMeta> {
            self.uploads
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let meta = self.inner.upload_immutable(local, key).await?;
            if let Some(after_upload) = &self.after_upload {
                after_upload()?;
            }
            if self
                .fail_pack
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                return Err(StateError::Storage(
                    "injected lost pack acknowledgement".into(),
                ));
            }
            Ok(meta)
        }
        async fn download(&self, key: &noland_storage::RemoteKey, dest: &Path) -> Result<()> {
            self.inner.download(key, dest).await
        }
        async fn list_prefix(
            &self,
            key: &noland_storage::RemoteKey,
        ) -> Result<Vec<noland_storage::RemoteEntry>> {
            self.inner.list_prefix(key).await
        }
        async fn put_small_versioned(
            &self,
            bytes: bytes::Bytes,
            key: &noland_storage::RemoteKey,
        ) -> Result<noland_storage::RemoteMeta> {
            if key.as_str().starts_with("catalog/")
                && self
                    .fail_catalog
                    .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                return Err(StateError::Storage("injected catalog interruption".into()));
            }
            if key.as_str().ends_with("manifest.enc")
                && self.fail.swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                return Err(StateError::Storage(
                    "injected interruption after packs uploaded".into(),
                ));
            }
            self.inner.put_small_versioned(bytes, key).await
        }
        fn storage_identity(&self) -> Option<ProviderRootIdentity> {
            self.inner.storage_identity()
        }
    }

    #[tokio::test]
    async fn retry_preserves_pack_ids_and_captured_saves_without_reuploading() {
        let root = test_root("retry-immutable");
        let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
        let app_id = AppId::desktop("retry-game");
        agent
            .db
            .upsert_app(&AppIdentity::new(app_id.clone(), "Retry Fixture"))
            .unwrap();
        let (path, _) = tracked_save(&agent, &app_id, "save.dat", b"captured save");
        let master = MasterKey::generate();
        let storage = InterruptedUpload {
            inner: LocalStorage::new(root.join("cloud")),
            fail: true.into(),
            fail_pack: false.into(),
            fail_catalog: false.into(),
            uploads: 0.into(),
            after_upload: None,
        };
        let operation_id = Uuid::new_v4();
        assert!(run_backup(
            &agent,
            &app_id,
            BackupMode::PersonalState,
            BackupPerformanceMode::Balanced,
            &storage,
            &master,
            Some(operation_id)
        )
        .await
        .is_err());
        let prepared = read_prepared_backup(
            &agent,
            &storage,
            &master,
            operation_id,
            &app_id,
            BackupMode::PersonalState,
        )
        .unwrap()
        .unwrap();
        let uploaded = storage.uploads.load(std::sync::atomic::Ordering::SeqCst);
        assert!(uploaded > 0);
        // Emulate a crash after persisting the sealed checkpoint, before removing
        // its lease. An independent capture must remain pinned during recovery.
        let cas = LocalCas::new(agent.config.paths.cache.join("cas/chunks")).unwrap();
        let active = cas.lease().unwrap();
        let hash = noland_cas::blake3_hex(b"captured save");
        let active_chunk = active.put_prehashed(&hash, b"captured save").unwrap();
        let retired_lease = prepared.release_lease.as_ref().unwrap();
        std::fs::create_dir_all(retired_lease).unwrap();
        std::fs::hard_link(
            &active_chunk.path,
            retired_lease.join(active_chunk.path.file_name().unwrap()),
        )
        .unwrap();
        std::fs::write(&path, b"newer local save must survive retry").unwrap();
        let resumed = run_backup(
            &agent,
            &app_id,
            BackupMode::PersonalState,
            BackupPerformanceMode::Balanced,
            &storage,
            &master,
            Some(operation_id),
        )
        .await
        .unwrap();
        assert_eq!(resumed.bundle_id, prepared.manifest.bundle_id);
        assert_eq!(
            storage.uploads.load(std::sync::atomic::Ordering::SeqCst),
            uploaded
        );
        assert_eq!(
            resumed.files[0].file_hash,
            noland_cas::blake3_hex(b"captured save")
        );
        assert_eq!(
            std::fs::read(path).unwrap(),
            b"newer local save must survive retry"
        );
        assert!(agent
            .db
            .list_dirty_apps()
            .unwrap()
            .iter()
            .any(|dirty| dirty.app_id == app_id));
        assert!(!prepared_backup_path(&agent, operation_id).exists());
        assert!(!retired_lease.exists());
        cas.evict_to(0).unwrap();
        assert_eq!(std::fs::read(&active_chunk.path).unwrap(), b"captured save");
        drop(active);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn writes_during_upload_stay_pending_for_the_next_capture() {
        let root = test_root("concurrent-save");
        let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
        let app = AppId::desktop("concurrent-save");
        agent
            .db
            .upsert_app(&AppIdentity::new(app.clone(), "Concurrent Save"))
            .unwrap();
        let (path, path_id) = tracked_save(&agent, &app, "save.dat", b"save-v1");
        let mutation =
            AppMutationRecord::new(app.clone(), path.to_string_lossy(), AppMutationKind::Modify);
        agent.db.append_app_mutation(&mutation).unwrap();
        agent.db.mark_dirty(&app, Some(path_id), false).unwrap();
        let db = agent.db.clone();
        let changed_app = app.clone();
        let changed_path = path.clone();
        let storage = InterruptedUpload {
            inner: LocalStorage::new(root.join("cloud")),
            fail: false.into(),
            fail_pack: false.into(),
            fail_catalog: false.into(),
            uploads: 0.into(),
            after_upload: Some(Box::new(move || {
                std::fs::write(&changed_path, b"save-v2")?;
                let mut later = AppMutationRecord::new(
                    changed_app.clone(),
                    changed_path.to_string_lossy(),
                    AppMutationKind::Modify,
                );
                // Coalesce into the captured row even within the same millisecond.
                later.observed_at = mutation.observed_at;
                db.append_app_mutation(&later)?;
                db.mark_dirty(&changed_app, Some(path_id), false)?;
                Ok(())
            })),
        };
        let key = MasterKey::generate();
        let captured = run_backup(
            &agent,
            &app,
            BackupMode::PersonalState,
            BackupPerformanceMode::Balanced,
            &storage,
            &key,
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            captured.files[0].file_hash,
            noland_cas::blake3_hex(b"save-v1")
        );
        assert_eq!(agent.db.count_pending_app_mutations(&app).unwrap(), 1);
        assert!(agent
            .db
            .list_dirty_apps()
            .unwrap()
            .iter()
            .any(|dirty| dirty.app_id == app));
        let next = run_backup(
            &agent,
            &app,
            BackupMode::PersonalState,
            BackupPerformanceMode::Balanced,
            &storage.inner,
            &key,
            None,
        )
        .await
        .unwrap();
        assert_eq!(next.files[0].file_hash, noland_cas::blake3_hex(b"save-v2"));
        assert_eq!(agent.db.count_pending_app_mutations(&app).unwrap(), 0);
        assert!(!agent
            .db
            .list_dirty_apps()
            .unwrap()
            .iter()
            .any(|dirty| dirty.app_id == app));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn restart_rehashes_same_identity_saves_changed_while_observer_was_offline() {
        let root = test_root("restart-trust");
        let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
        let app = AppId::desktop("restart-trust");
        agent
            .db
            .upsert_app(&AppIdentity::new(app.clone(), "Restart Trust"))
            .unwrap();
        let (path, _) = tracked_save(&agent, &app, "save.dat", b"save-v1");
        let storage = LocalStorage::new(root.join("cloud"));
        let key = MasterKey::generate();
        let first = run_backup(
            &agent,
            &app,
            BackupMode::PersonalState,
            BackupPerformanceMode::Balanced,
            &storage,
            &key,
            None,
        )
        .await
        .unwrap();
        assert!(agent.db.list_dirty_apps().unwrap().is_empty());
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        drop(agent);
        std::fs::write(&path, b"save-v2").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let restarted = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
        restarted.recover().unwrap();
        let operation = Uuid::new_v4();
        let next = run_backup(
            &restarted,
            &app,
            BackupMode::PersonalState,
            BackupPerformanceMode::Balanced,
            &storage,
            &key,
            Some(operation),
        )
        .await
        .unwrap();
        assert_ne!(first.bundle_id, next.bundle_id);
        assert_eq!(next.files[0].file_hash, noland_cas::blake3_hex(b"save-v2"));
        assert_eq!(
            operation_metrics(&restarted.db.get_operation(operation).unwrap().unwrap())
                .num_files_rehashed,
            1
        );
        drop(restarted);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn corrupted_retry_inputs_fail_before_publication_and_remain_recoverable() {
        let root = test_root("retry-integrity");
        let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
        let app = AppId::desktop("retry-integrity");
        agent
            .db
            .upsert_app(&AppIdentity::new(app.clone(), "Retry Integrity"))
            .unwrap();
        let (save, _) = tracked_save(&agent, &app, "save.dat", b"preserve this save");
        let storage = InterruptedUpload {
            inner: LocalStorage::new(root.join("cloud")),
            fail: false.into(),
            fail_pack: true.into(),
            fail_catalog: false.into(),
            uploads: 0.into(),
            after_upload: None,
        };
        let key = MasterKey::generate();
        let operation = Uuid::new_v4();
        assert!(run_backup(
            &agent,
            &app,
            BackupMode::PersonalState,
            BackupPerformanceMode::Balanced,
            &storage,
            &key,
            Some(operation)
        )
        .await
        .is_err());
        let prepared = read_prepared_backup(
            &agent,
            &storage,
            &key,
            operation,
            &app,
            BackupMode::PersonalState,
        )
        .unwrap()
        .unwrap();
        let checkpoint = prepared_backup_path(&agent, operation);
        let receipt = checkpoint.with_extension("packs").join("0.enc");
        let directory = agent
            .config
            .paths
            .packs
            .join(prepared.manifest.bundle_id.to_string());
        let pack = std::fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.extension().is_some_and(|ext| ext == "pack"))
            .unwrap();
        let uploaded = storage.uploads.load(std::sync::atomic::Ordering::SeqCst);
        let committed = root
            .join("cloud")
            .join(bundle_dir(&app, prepared.manifest.bundle_id))
            .join(COMMITTED_MARKER);
        for artifact in [checkpoint, receipt, pack] {
            let bytes = std::fs::read(&artifact).unwrap();
            let mut corrupted = bytes.clone();
            let middle = corrupted.len() / 2;
            corrupted[middle] ^= 1;
            std::fs::write(&artifact, corrupted).unwrap();
            assert!(run_backup(
                &agent,
                &app,
                BackupMode::PersonalState,
                BackupPerformanceMode::Balanced,
                &storage,
                &key,
                Some(operation)
            )
            .await
            .is_err());
            assert!(!committed.exists());
            assert_eq!(
                storage.uploads.load(std::sync::atomic::Ordering::SeqCst),
                uploaded
            );
            std::fs::write(artifact, bytes).unwrap();
        }
        assert!(read_prepared_backup(
            &agent,
            &storage,
            &MasterKey::generate(),
            operation,
            &app,
            BackupMode::PersonalState
        )
        .is_err());
        assert!(read_prepared_backup(
            &agent,
            &LocalStorage::new(root.join("other-cloud")),
            &key,
            operation,
            &app,
            BackupMode::PersonalState
        )
        .is_err());
        let resumed = run_backup(
            &agent,
            &app,
            BackupMode::PersonalState,
            BackupPerformanceMode::Balanced,
            &storage,
            &key,
            Some(operation),
        )
        .await
        .unwrap();
        assert_eq!(resumed.bundle_id, prepared.manifest.bundle_id);
        assert_eq!(std::fs::read(save).unwrap(), b"preserve this save");
        assert!(committed.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn retries_resume_pack_receipts_and_preserve_already_committed_ciphertext() {
        for fail_pack in [true, false] {
            let root = test_root("retry-pipeline");
            let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
            let app_id = AppId::desktop("pipeline-game");
            agent
                .db
                .upsert_app(&AppIdentity::new(app_id.clone(), "Pipeline Fixture"))
                .unwrap();
            // Three small-file pack plans exercise a failed upload while other
            // sealing workers are active, rather than only a one-pack retry.
            for index in 0..if fail_pack { 132 } else { 32 } {
                let bytes = if fail_pack {
                    vec![index as u8; 256 * 1024]
                } else {
                    format!("captured-{index}").into_bytes()
                };
                tracked_save(&agent, &app_id, &format!("slot-{index}.sav"), &bytes);
            }
            let storage = InterruptedUpload {
                inner: LocalStorage::new(root.join("cloud")),
                fail: false.into(),
                fail_pack: fail_pack.into(),
                fail_catalog: (!fail_pack).into(),
                uploads: 0.into(),
                after_upload: None,
            };
            let master = MasterKey::generate();
            let operation = Uuid::new_v4();
            assert!(run_backup(
                &agent,
                &app_id,
                BackupMode::PersonalState,
                BackupPerformanceMode::Balanced,
                &storage,
                &master,
                Some(operation)
            )
            .await
            .is_err());
            let prepared = read_prepared_backup(
                &agent,
                &storage,
                &master,
                operation,
                &app_id,
                BackupMode::PersonalState,
            )
            .unwrap()
            .unwrap();
            assert_eq!(prepared.packing.is_some(), fail_pack);
            let expected_packs = prepared
                .packing
                .as_ref()
                .map_or(prepared.pack_files.len(), |packing| packing.plans.len());
            if fail_pack {
                assert!(expected_packs > 1);
            }
            let sealed_before = std::fs::read_dir(
                agent
                    .config
                    .paths
                    .packs
                    .join(prepared.manifest.bundle_id.to_string()),
            )
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<BTreeSet<_>>();
            let remote = root.join("cloud");
            let packs_before = storage
                .list_prefix(&noland_storage::RemoteKey::new("packs/"))
                .await
                .unwrap()
                .into_iter()
                .filter(|entry| !entry.is_prefix)
                .map(|entry| PathBuf::from(entry.key.as_str()))
                .collect::<BTreeSet<_>>();
            let manifest_path = remote
                .join(bundle_dir(&app_id, prepared.manifest.bundle_id))
                .join("manifest.enc");
            let previous_ciphertext = std::fs::read(&manifest_path).ok();
            let resumed = run_backup(
                &agent,
                &app_id,
                BackupMode::PersonalState,
                BackupPerformanceMode::Balanced,
                &storage,
                &master,
                Some(operation),
            )
            .await
            .unwrap();
            assert_eq!(resumed.bundle_id, prepared.manifest.bundle_id);
            let packs_after = storage
                .list_prefix(&noland_storage::RemoteKey::new("packs/"))
                .await
                .unwrap()
                .into_iter()
                .filter(|entry| !entry.is_prefix)
                .map(|entry| PathBuf::from(entry.key.as_str()))
                .collect::<BTreeSet<_>>();
            assert!(
                packs_before.is_subset(&packs_after),
                "retry must keep uploaded pack IDs"
            );
            let remote_names = packs_after
                .iter()
                .map(|path| path.file_name().unwrap().to_owned())
                .collect::<BTreeSet<_>>();
            assert!(
                sealed_before.is_subset(&remote_names),
                "retry must keep locally sealed pack IDs too"
            );
            assert_eq!(packs_after.len(), expected_packs);
            if let Some(before) = previous_ciphertext {
                assert_eq!(std::fs::read(manifest_path).unwrap(), before);
            }
            assert!(
                std::fs::read_dir(agent.config.paths.cache.join("cas/chunks/.leases"))
                    .unwrap()
                    .next()
                    .is_none()
            );
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    fn test_root(label: &str) -> PathBuf {
        std::env::current_dir()
            .expect("test working directory should be available")
            .join("target/noland-state-agent-tests")
            .join(format!("backup-{label}-{}", Uuid::new_v4()))
    }

    #[test]
    fn local_packs_are_removed_only_after_cloud_commit() {
        let retained = test_root("packs-retained-on-failure");
        std::fs::create_dir_all(&retained).unwrap();
        std::fs::write(retained.join("pack"), b"data").unwrap();
        {
            let _cleanup = PackCleanup::new(retained.clone());
        }
        assert!(retained.join("pack").is_file());
        std::fs::remove_dir_all(&retained).unwrap();

        let committed = test_root("packs-removed-after-commit");
        std::fs::create_dir_all(&committed).unwrap();
        std::fs::write(committed.join("pack"), b"data").unwrap();
        {
            let mut cleanup = PackCleanup::new(committed.clone());
            cleanup.mark_cloud_committed();
        }
        assert!(!committed.exists());
    }

    fn tracked_save(
        agent: &StateAgent,
        app_id: &AppId,
        relative: &str,
        bytes: &[u8],
    ) -> (PathBuf, i64) {
        let path = agent.config.home.join(".config/test-game").join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        let canonical = path.to_string_lossy().into_owned();
        let path_id = agent.db.upsert_path(&canonical).unwrap();
        let now = Utc::now();
        agent
            .db
            .upsert_association(&PathAssociation {
                app_id: app_id.clone(),
                path_id,
                confidence: CONF_EXPLICIT,
                evidence: vec![Evidence::new(EvidenceKind::DirectCgroupWrite)],
                persistence_class: PersistenceClass::PersistentState,
                semantic_role: SemanticRole::UserState,
                first_seen_at: now,
                last_seen_at: now,
            })
            .unwrap();
        (path, path_id)
    }

    fn associate_path(
        agent: &StateAgent,
        app_id: &AppId,
        path: &Path,
        evidence: EvidenceKind,
    ) -> i64 {
        let path_id = agent.db.upsert_path(&path.to_string_lossy()).unwrap();
        agent
            .db
            .upsert_association(&PathAssociation {
                app_id: app_id.clone(),
                path_id,
                confidence: CONF_EXPLICIT,
                evidence: vec![Evidence::new(evidence)],
                persistence_class: PersistenceClass::PersistentState,
                semantic_role: SemanticRole::UserState,
                first_seen_at: Utc::now(),
                last_seen_at: Utc::now(),
            })
            .unwrap();
        path_id
    }

    fn mark_mutated(
        agent: &StateAgent,
        app_id: &AppId,
        path: &Path,
        path_id: i64,
        kind: AppMutationKind,
    ) {
        agent
            .db
            .append_app_mutation(&AppMutationRecord::new(
                app_id.clone(),
                path.to_string_lossy(),
                kind,
            ))
            .unwrap();
        agent.db.mark_dirty(app_id, Some(path_id), false).unwrap();
        agent
            .db
            .mark_dirty_root(
                app_id,
                path.parent().unwrap().to_string_lossy().as_ref(),
                Some("$XDG_CONFIG_HOME"),
                false,
            )
            .unwrap();
    }

    #[tokio::test]
    async fn personal_backup_excludes_dated_server_logs_and_keeps_saves() {
        let root = test_root("dated-server-log-exclusion");
        let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
        let app = AppIdentity::new(AppId::desktop("test-game"), "Test Game");
        agent.db.upsert_app(&app).unwrap();
        let log = agent
            .config
            .home
            .join("server/logs/localhost_access_log.2026-11-09.txt");
        let save = agent.config.home.join("game/user/save/progress.sav");
        for (path, bytes) in [
            (&log, b"diagnostic".as_slice()),
            (&save, b"save data".as_slice()),
        ] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
            associate_path(&agent, &app.app_id, path, EvidenceKind::DirectCgroupWrite);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o000)).unwrap();
        }
        let manifest = run_backup_to_local(
            &agent,
            &app.app_id,
            BackupMode::PersonalState,
            root.join("cloud"),
            &MasterKey::generate(),
        )
        .await
        .unwrap();
        assert_eq!(manifest.files.len(), 1);
        assert_eq!(
            manifest.files[0].source_path_hint.as_deref(),
            Some(save.to_string_lossy().as_ref())
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn complete_backup_includes_private_runtime_application_files() {
        let root = test_root("private-runtime-complete");
        let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
        let game = agent
            .config
            .home
            .join(".local/share/noland-runtime/app-data/runtime-app/Games/Hookshot");
        std::fs::create_dir_all(game.join("bin")).unwrap();
        std::fs::write(game.join("bin/hookshot"), b"executable").unwrap();
        std::fs::write(game.join("game.data"), b"game assets").unwrap();
        let other = agent
            .config
            .home
            .join(".local/share/noland-runtime/app-data/other-app/Games/Other/private.dat");
        std::fs::create_dir_all(other.parent().unwrap()).unwrap();
        std::fs::write(&other, b"other app").unwrap();
        let app = noland_discovery::fallback_exe_identity(Path::new(
            "/home/runtime/Games/Hookshot/bin/hookshot",
        ));
        agent.db.upsert_app(&app).unwrap();
        agent.discover().unwrap();
        let manifest = run_backup_to_local(
            &agent,
            &app.app_id,
            BackupMode::CompleteApplication,
            root.join("cloud"),
            &MasterKey::generate(),
        )
        .await
        .unwrap();
        assert_eq!(manifest.files.len(), 2);
        assert!(manifest.files.iter().all(|file| file
            .source_path_hint
            .as_deref()
            .is_some_and(|path| Path::new(path).starts_with(&game))));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn backup_rejects_directly_indexed_agent_storage() {
        let root = test_root("self-exclusion");
        let cloud = root.join("cloud");
        let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
        let app = AppIdentity::new(AppId::desktop("test-game"), "Test Game");
        let app_id = app.app_id.clone();
        agent.db.upsert_app(&app).unwrap();

        let internal = agent.config.paths.state_root.join("should-not-back-up.dat");
        std::fs::write(&internal, b"internal").unwrap();
        let path_id = agent
            .db
            .upsert_path(internal.to_string_lossy().as_ref())
            .unwrap();
        let now = Utc::now();
        agent
            .db
            .upsert_association(&PathAssociation {
                app_id: app_id.clone(),
                path_id,
                confidence: CONF_EXPLICIT,
                evidence: vec![Evidence::new(EvidenceKind::ExplicitUserBinding)],
                persistence_class: PersistenceClass::PersistentState,
                semantic_role: SemanticRole::UserState,
                first_seen_at: now,
                last_seen_at: now,
            })
            .unwrap();

        let error = run_backup_to_local(
            &agent,
            &app_id,
            BackupMode::CompleteApplication,
            cloud,
            &MasterKey::generate(),
        )
        .await
        .expect_err("agent-owned storage must never become backup content");
        assert!(
            error.to_string().contains("contains no application files"),
            "unexpected error: {error}"
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn incremental_backup_rehashes_only_changed_file_and_preserves_parent_index() {
        let root = test_root("incremental");
        let cloud = root.join("cloud");
        let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
        let app = AppIdentity::new(AppId::desktop("test-game"), "Test Game");
        let app_id = app.app_id.clone();
        agent.db.upsert_app(&app).unwrap();
        let (first_path, first_id) =
            tracked_save(&agent, &app_id, "Mcd001.ps2", b"memory-card-one-v1");
        let _ = tracked_save(&agent, &app_id, "Mcd002.ps2", b"memory-card-two-v1");
        let master = MasterKey::generate();

        let first = run_backup_to_local(
            &agent,
            &app_id,
            BackupMode::PersonalState,
            cloud.clone(),
            &master,
        )
        .await
        .unwrap();
        assert_eq!(first.files.len(), 2);

        std::fs::write(&first_path, b"memory-card-one-v2").unwrap();
        mark_mutated(
            &agent,
            &app_id,
            &first_path,
            first_id,
            AppMutationKind::Modify,
        );
        let second = run_backup_to_local(
            &agent,
            &app_id,
            BackupMode::PersonalState,
            cloud.clone(),
            &master,
        )
        .await
        .unwrap();
        assert_eq!(second.parent_bundle_id, Some(first.bundle_id));
        assert_eq!(second.files.len(), 2);

        let operation = agent
            .db
            .recent_operations(10)
            .unwrap()
            .into_iter()
            .find(|operation| {
                operation.detail_json["bundle_id"] == serde_json::json!(second.bundle_id)
            })
            .unwrap();
        let metrics = operation_metrics(&operation);
        assert_eq!(metrics.num_candidate_paths, 1);
        assert_eq!(metrics.num_files_rehashed, 1);
        assert_eq!(metrics.num_files_skipped_fast_identity, 0);

        let storage = LocalStorage::new(cloud.clone());
        let index = read_pack_index(&storage, &master, &app_id, second.bundle_id)
            .await
            .unwrap();
        let indexed = index
            .iter()
            .map(|entry| entry.chunk_hash.as_str())
            .collect::<BTreeSet<_>>();
        for file in &second.files {
            for chunk in &file.chunks {
                assert!(indexed.contains(chunk.hash.as_str()));
            }
        }

        let no_op = run_backup_to_local(&agent, &app_id, BackupMode::PersonalState, cloud, &master)
            .await
            .unwrap();
        assert_eq!(no_op.bundle_id, second.bundle_id);
        let operation = agent
            .db
            .recent_operations(10)
            .unwrap()
            .into_iter()
            .find(|operation| operation.detail_json["no_op"] == true)
            .unwrap();
        let metrics = operation_metrics(&operation);
        assert_eq!(metrics.num_candidate_paths, 0);
        assert_eq!(metrics.num_files_rehashed, 0);
        assert_eq!(operation.detail_json["no_op"], true);
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn deletion_creates_tombstone_and_clears_evidence_only_after_commit() {
        let root = test_root("delete");
        let cloud = root.join("cloud");
        let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
        let app = AppIdentity::new(AppId::desktop("test-game"), "Test Game");
        let app_id = app.app_id.clone();
        agent.db.upsert_app(&app).unwrap();
        let (path, path_id) = tracked_save(&agent, &app_id, "Mcd001.ps2", b"memory-card");
        let master = MasterKey::generate();
        let first = run_backup_to_local(
            &agent,
            &app_id,
            BackupMode::PersonalState,
            cloud.clone(),
            &master,
        )
        .await
        .unwrap();

        std::fs::remove_file(&path).unwrap();
        mark_mutated(&agent, &app_id, &path, path_id, AppMutationKind::Delete);
        assert_eq!(
            agent.db.pending_app_mutations(&app_id, 10).unwrap().len(),
            1
        );
        let second =
            run_backup_to_local(&agent, &app_id, BackupMode::PersonalState, cloud, &master)
                .await
                .unwrap();
        assert_eq!(second.parent_bundle_id, Some(first.bundle_id));
        assert!(second.files.is_empty());
        assert_eq!(second.tombstones.len(), 1);
        assert_eq!(second.tombstones[0].relative_path, "test-game/Mcd001.ps2");
        assert!(agent
            .db
            .pending_app_mutations(&app_id, 10)
            .unwrap()
            .is_empty());
        assert!(agent.db.list_dirty_apps().unwrap().is_empty());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn finds_manifest_in_registered_steam_library() {
        let root = test_root("steam-manifest-tracking");
        let steamapps = root.join("steamapps");
        std::fs::create_dir_all(&steamapps).unwrap();
        let manifest = steamapps.join("appmanifest_3241660.acf");
        std::fs::write(&manifest, b"appmanifest").unwrap();

        let mut roots = LogicalRootMap::default();
        roots.steam_libraries.insert("0".into(), steamapps);

        assert_eq!(steam_appmanifest_path(3241660, &roots, &[]), Some(manifest));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn manifest_selection_follows_the_selected_install_library() {
        let root = test_root("steam-selected-manifest");
        let first_steamapps = root.join("first/steamapps");
        let selected_steamapps = root.join("selected/steamapps");
        let install = selected_steamapps.join("common/Test Game");
        let stale_manifest = first_steamapps.join("appmanifest_123.acf");
        let selected_manifest = selected_steamapps.join("appmanifest_123.acf");
        for manifest in [&stale_manifest, &selected_manifest] {
            std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
            std::fs::write(manifest, b"manifest").unwrap();
        }

        let mut roots = LogicalRootMap::default();
        roots
            .steam_libraries
            .insert("first".into(), first_steamapps);
        roots
            .steam_libraries
            .insert("selected".into(), selected_steamapps);

        assert_eq!(
            steam_appmanifest_path(123, &roots, &[install]),
            Some(selected_manifest)
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn personal_state_does_not_reconcile_unobserved_install_content() {
        let root = test_root("personal-known-root");
        let cloud = root.join("cloud");
        let install = root.join("library/game");
        let content = install.join("game.bin");
        std::fs::create_dir_all(&install).unwrap();
        std::fs::write(&content, b"game executable").unwrap();

        let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
        let app = AppIdentity::new(AppId::desktop("test-game"), "Test Game");
        let app_id = app.app_id.clone();
        agent.db.upsert_app(&app).unwrap();
        agent
            .db
            .add_known_root(&app_id, "install", install.to_string_lossy().as_ref())
            .unwrap();

        let manifest = run_backup_to_local(
            &agent,
            &app_id,
            BackupMode::PersonalState,
            cloud,
            &MasterKey::generate(),
        )
        .await
        .unwrap();

        assert!(manifest.files.is_empty());
        assert!(agent
            .db
            .get_path_by_canonical(content.to_string_lossy().as_ref())
            .unwrap()
            .is_none());
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn complete_application_reconciles_unobserved_install_content_and_deep_trees() {
        let root = test_root("complete-known-root");
        let cloud = root.join("cloud");
        let steamapps = root.join("library/steamapps");
        let install = steamapps.join("common/Test Game");
        let shallow = install.join("game.bin");
        let deep = install
            .join("one/two/three/four/five/six/seven/eight")
            .join("asset.pak");
        std::fs::create_dir_all(deep.parent().unwrap()).unwrap();
        std::fs::write(&shallow, b"game executable").unwrap();
        std::fs::write(&deep, b"deep install asset").unwrap();
        std::fs::write(steamapps.join("appmanifest_123.acf"), b"manifest").unwrap();

        let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
        let mut app = AppIdentity::new(AppId::steam(123), "Test Game");
        app.steam_app_id = Some(123);
        let app_id = app.app_id.clone();
        agent.db.upsert_app(&app).unwrap();
        agent
            .db
            .add_known_root(&app_id, "install", install.to_string_lossy().as_ref())
            .unwrap();
        agent
            .roots
            .lock()
            .steam_libraries
            .insert("test".into(), steamapps);
        assert!(agent
            .db
            .get_path_by_canonical(shallow.to_string_lossy().as_ref())
            .unwrap()
            .is_none());
        assert!(agent
            .db
            .get_path_by_canonical(deep.to_string_lossy().as_ref())
            .unwrap()
            .is_none());

        let manifest = run_backup_to_local(
            &agent,
            &app_id,
            BackupMode::CompleteApplication,
            cloud,
            &MasterKey::generate(),
        )
        .await
        .unwrap();
        let source_paths = manifest
            .files
            .iter()
            .filter_map(|file| file.source_path_hint.as_deref())
            .collect::<BTreeSet<_>>();
        assert!(source_paths.contains(shallow.to_string_lossy().as_ref()));
        assert!(source_paths.contains(deep.to_string_lossy().as_ref()));
        assert!(source_paths
            .iter()
            .any(|path| path.ends_with("appmanifest_123.acf")));

        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn steam_complete_backup_includes_only_install_and_explicit_state() {
        let root = test_root("steam-strict-scope");
        let cloud = root.join("cloud");
        let home = root.join("home");
        let steamapps = home.join(".local/share/Steam/steamapps");
        let install = steamapps.join("common/Test Game");
        let install_file = install.join("game.bin");
        let deep_install_file = install.join("data/content.pak");
        let compatdata_file =
            steamapps.join("compatdata/123/pfx/drive_c/windows/system32/runtime.dll");
        let proton_file = steamapps.join("common/Proton 9.0/proton");
        let linux_runtime_file = steamapps.join("common/SteamLinuxRuntime_sniper/run");
        let steamworks_file = steamapps.join("common/Steamworks Shared/redist.bin");
        let other_game_file = steamapps.join("common/Other Game/other.bin");
        let shader_file = steamapps.join("shadercache/123/cache.bin");
        let unrelated_file = home.join(".local/share/unrelated/data.bin");
        let explicit_save_root = steamapps.join("compatdata/123/pfx/save");
        let explicit_save = explicit_save_root.join("save.sav");
        let explicit_config = steamapps.join("compatdata/123/pfx/config");
        let explicit_config_file = explicit_config.join("settings.json");
        let appmanifest = steamapps.join("appmanifest_123.acf");
        let other_appmanifest = steamapps.join("appmanifest_999.acf");
        for file in [
            &install_file,
            &deep_install_file,
            &compatdata_file,
            &proton_file,
            &linux_runtime_file,
            &steamworks_file,
            &other_game_file,
            &shader_file,
            &unrelated_file,
            &explicit_save,
            &explicit_config_file,
            &appmanifest,
            &other_appmanifest,
        ] {
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, b"data").unwrap();
        }

        let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
        let mut app = AppIdentity::new(AppId::steam(123), "Test Game");
        app.steam_app_id = Some(123);
        let app_id = app.app_id.clone();
        agent.db.upsert_app(&app).unwrap();
        let broad_common_root = steamapps.join("common");
        for (kind, path) in [
            ("install", &install),
            ("install", &broad_common_root),
            ("proton", &steamapps.join("compatdata/123/pfx")),
            ("install", &steamapps.join("common/Proton 9.0")),
            (
                "install",
                &steamapps.join("common/SteamLinuxRuntime_sniper"),
            ),
            ("install", &steamapps.join("common/Steamworks Shared")),
        ] {
            agent
                .db
                .add_known_root(&app_id, kind, &path.to_string_lossy())
                .unwrap();
        }
        agent
            .roots
            .lock()
            .steam_libraries
            .insert("test".into(), steamapps.clone());

        associate_path(
            &agent,
            &app_id,
            &steamapps.join("compatdata/123/pfx"),
            EvidenceKind::ProtonPrefix,
        );
        for path in [
            &proton_file,
            &linux_runtime_file,
            &steamworks_file,
            &other_game_file,
            &shader_file,
            &unrelated_file,
            Path::new("/usr/lib/libc.so.6"),
        ] {
            associate_path(&agent, &app_id, path, EvidenceKind::ReadOnlyDependency);
        }
        associate_path(
            &agent,
            &app_id,
            &explicit_save_root,
            EvidenceKind::ExplicitUserBinding,
        );
        associate_path(
            &agent,
            &app_id,
            &explicit_config,
            EvidenceKind::ExplicitUserBinding,
        );

        let manifest = run_backup_to_local(
            &agent,
            &app_id,
            BackupMode::CompleteApplication,
            cloud,
            &MasterKey::generate(),
        )
        .await
        .unwrap();
        let source_paths = manifest
            .files
            .iter()
            .filter_map(|file| file.source_path_hint.clone())
            .collect::<BTreeSet<_>>();
        let expected = [
            install_file.to_string_lossy().into_owned(),
            deep_install_file.to_string_lossy().into_owned(),
            explicit_save.to_string_lossy().into_owned(),
            explicit_config_file.to_string_lossy().into_owned(),
            appmanifest.to_string_lossy().into_owned(),
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();
        assert_eq!(source_paths, expected);
        assert!(manifest
            .files
            .iter()
            .any(|file| file.relative_path.ends_with("appmanifest_123.acf")));
        assert!(!manifest
            .files
            .iter()
            .any(|file| file.relative_path.ends_with("appmanifest_999.acf")));

        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn steam_complete_backup_prunes_inherited_files_outside_scope() {
        let root = test_root("steam-parent-prune");
        let cloud = root.join("cloud");
        let home = root.join("home");
        let steamapps = home.join(".local/share/Steam/steamapps");
        let install = steamapps.join("common/Test Game");
        let install_file = install.join("game.bin");
        let leaked_runtime = steamapps.join("compatdata/123/pfx/drive_c/runtime.dll");
        for file in [&install_file, &leaked_runtime] {
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, b"data").unwrap();
        }

        let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
        let mut app = AppIdentity::new(AppId::steam(123), "Test Game");
        app.steam_app_id = Some(123);
        let app_id = app.app_id.clone();
        agent.db.upsert_app(&app).unwrap();
        agent
            .db
            .add_known_root(&app_id, "install", &install.to_string_lossy())
            .unwrap();
        associate_path(
            &agent,
            &app_id,
            &leaked_runtime,
            EvidenceKind::DirectCgroupWrite,
        );
        let master = MasterKey::generate();

        let parent = run_backup_to_local(
            &agent,
            &app_id,
            BackupMode::PersonalState,
            cloud.clone(),
            &master,
        )
        .await
        .unwrap();
        assert!(parent.files.iter().any(|file| {
            file.source_path_hint.as_deref() == Some(leaked_runtime.to_string_lossy().as_ref())
        }));

        let strict = run_backup_to_local(
            &agent,
            &app_id,
            BackupMode::CompleteApplication,
            cloud,
            &master,
        )
        .await
        .unwrap();
        assert_eq!(strict.parent_bundle_id, Some(parent.bundle_id));
        assert!(!strict.files.iter().any(|file| {
            file.source_path_hint.as_deref() == Some(leaked_runtime.to_string_lossy().as_ref())
        }));
        assert!(strict.tombstones.iter().any(|tombstone| {
            tombstone.reason == "outside strict Steam backup scope"
                && tombstone.relative_path.ends_with("drive_c/runtime.dll")
        }));

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn single_pack_worker_builds_every_planned_pack() {
        let root = test_root("single-pack-worker");
        let chunks_dir = root.join("chunks");
        let packs_dir = root.join("packs");
        std::fs::create_dir_all(&chunks_dir).unwrap();
        let mut chunks = Vec::new();
        for index in 0..4 {
            let path = chunks_dir.join(format!("chunk-{index}"));
            std::fs::write(&path, [index as u8; 10]).unwrap();
            chunks.push((format!("chunk-hash-{index}"), path));
        }

        let packs = build_packs_parallel(
            &packs_dir,
            &MasterKey::generate(),
            chunks,
            86,
            100,
            1,
            Arc::new(|| false),
        )
        .unwrap();

        assert_eq!(packs.len(), 4);
        assert_eq!(
            packs.iter().map(|pack| pack.entries.len()).sum::<usize>(),
            4
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn remote_content_index_reuses_chunks_when_parent_index_is_absent() {
        let root = test_root("remote-reuse");
        let cloud = root.join("cloud");
        let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
        let app = AppIdentity::new(AppId::desktop("test-game"), "Test Game");
        let app_id = app.app_id.clone();
        agent.db.upsert_app(&app).unwrap();
        let (path, path_id) = tracked_save(&agent, &app_id, "Mcd001.ps2", b"memory-card-same");
        let master = MasterKey::generate();
        let first = run_backup_to_local(
            &agent,
            &app_id,
            BackupMode::PersonalState,
            cloud.clone(),
            &master,
        )
        .await
        .unwrap();
        let storage = LocalStorage::new(cloud.clone());
        let index = read_pack_index(&storage, &master, &app_id, first.bundle_id)
            .await
            .unwrap();
        remember_remote_pack_index(&agent, storage.storage_identity(), &master, &index).unwrap();

        std::fs::remove_dir_all(parent_cache_dir(&agent, &app_id, first.bundle_id)).ok();
        agent
            .db
            .record_commit(
                Uuid::new_v4(),
                &app_id,
                Uuid::new_v4(),
                "missing-parent",
                None,
                CommitVisibility::Committed,
            )
            .unwrap();
        mark_mutated(&agent, &app_id, &path, path_id, AppMutationKind::Modify);
        let second =
            run_backup_to_local(&agent, &app_id, BackupMode::PersonalState, cloud, &master)
                .await
                .unwrap();
        let operation = agent
            .db
            .recent_operations(10)
            .unwrap()
            .into_iter()
            .find(|operation| {
                operation.detail_json["bundle_id"] == serde_json::json!(second.bundle_id)
            })
            .unwrap();
        let metrics = operation_metrics(&operation);
        assert!(
            metrics.num_remote_index_hits > 0,
            "expected remote pack-index hits, got {metrics:?}"
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn cache_reuse_is_bound_to_both_repository_and_vault_key() {
        let root = test_root("cache-scope");
        let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
        let app = AppId::desktop("cache-scope");
        agent
            .db
            .upsert_app(&AppIdentity::new(app.clone(), "Cache Scope"))
            .unwrap();
        tracked_save(&agent, &app, "save.dat", b"portable save");
        let key = MasterKey::generate();
        let first_storage = LocalStorage::new(root.join("first-cloud"));
        let second_storage = LocalStorage::new(root.join("second-cloud"));
        let first = run_backup(
            &agent,
            &app,
            BackupMode::PersonalState,
            BackupPerformanceMode::Balanced,
            &first_storage,
            &key,
            None,
        )
        .await
        .unwrap();
        let second = run_backup(
            &agent,
            &app,
            BackupMode::PersonalState,
            BackupPerformanceMode::Balanced,
            &second_storage,
            &key,
            None,
        )
        .await
        .unwrap();
        assert_ne!(first.bundle_id, second.bundle_id);
        assert_eq!(first.files[0].file_hash, second.files[0].file_hash);
        let index = read_pack_index(&second_storage, &key, &app, second.bundle_id)
            .await
            .unwrap();
        for entry in index {
            assert!(second_storage
                .stat(&noland_storage::RemoteKey::new(pack_key(&entry.pack_id)))
                .await
                .unwrap()
                .is_some());
        }
        let other_key = MasterKey::generate();
        let original_scope =
            repository_cache_binding(second_storage.storage_identity(), &key).unwrap();
        let other_scope =
            repository_cache_binding(second_storage.storage_identity(), &other_key).unwrap();
        assert_ne!(original_scope, other_scope);
        assert!(read_cached_parent(&agent, &app, second.bundle_id, Some(&other_scope)).is_none());
        assert!(
            load_remote_chunk_index(&agent, Some(&other_scope), Utc::now())
                .unwrap()
                .entries
                .is_empty()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
