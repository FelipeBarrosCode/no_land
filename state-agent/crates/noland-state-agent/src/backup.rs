use std::collections::{BTreeMap, BTreeSet, HashMap};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use noland_cas::{chunk_file_streaming, LocalCas};
use noland_classifier::Classifier;
use noland_crypto::MasterKey;
use noland_pack::{pack_chunk_files_with_limits, BuiltPack, PackIndexEntry};
use noland_rclone_adapter::{EphemeralRcloneSession, ProviderRootIdentity, TransferTuning};
use noland_snapshot::{create_view, discard};
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
    staged: PathBuf,
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
    source: PathBuf,
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
    mut chunks: Vec<(String, PathBuf)>,
    target: u64,
    max: u64,
) -> Result<Vec<Vec<(String, PathBuf)>>> {
    const PACK_HEADER_BYTES: u64 = 24;
    const RECORD_OVERHEAD_BYTES: u64 = 52;
    if target == 0 || target > max {
        return Err(StateError::Invalid("invalid pack size limits".into()));
    }
    chunks.sort_by(|left, right| left.0.cmp(&right.0));
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
    cas: &LocalCas,
    inherited_hashes: &std::collections::HashSet<String>,
    remote_chunks: &RemoteChunkIndex,
    observed_at: chrono::DateTime<Utc>,
) -> Result<HashResult> {
    let mut new_chunks = Vec::new();
    let mut remote_entries = Vec::new();
    let mut cas_observations = Vec::new();
    let mut metrics = HashMetricsDelta::default();
    let summary = chunk_file_streaming(&job.staged, |chunk, payload| {
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
            local_path: stored.path.to_string_lossy().into_owned(),
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
    let file_state = file_state_from_manifest(&job.app_id, &manifest_file, &current_record);
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
        source: job.source,
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
    cas: LocalCas,
    inherited_hashes: std::collections::HashSet<String>,
    remote_chunks: RemoteChunkIndex,
    observed_at: chrono::DateTime<Utc>,
    cancelled: CancellationCheck,
) -> Result<Vec<HashResult>> {
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
    let mut results = Vec::new();
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
        for result in result_rx {
            match result {
                Ok(result) => results.push(result),
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
    results.sort_by(|left, right| left.source.cmp(&right.source));
    Ok(results)
}

struct SnapshotCleanup(PathBuf);

impl Drop for SnapshotCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
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
    persist_operation(agent, &mut op, BackupState::Discovering, &metrics)?;

    let identity = agent
        .db
        .get_app(app_id)?
        .ok_or_else(|| StateError::NotFound(app_id.to_string()))?;
    let dirty_state = agent
        .db
        .list_dirty_apps()?
        .into_iter()
        .find(|dirty| dirty.app_id == *app_id);
    let pending_mutations = agent.db.pending_app_mutations(app_id, 10_000)?;
    metrics.num_dirty_paths = dirty_state
        .as_ref()
        .map(|dirty| dirty.dirty_paths.len() as u64)
        .unwrap_or(0);
    metrics.num_dirty_roots = agent.db.list_dirty_roots(Some(app_id))?.len() as u64;

    let parent = load_parent(provider, master, agent, app_id).await;
    if parent.is_some()
        && dirty_state.is_none()
        && pending_mutations.is_empty()
        && mode == BackupMode::PersonalState
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

    let mut tombstones = BTreeMap::<(String, String), String>::new();
    if let Some(scope) = strict_steam_scope.as_ref() {
        let inherited_files = std::mem::take(&mut manifest.files);
        for file in inherited_files {
            let in_scope = manifest_source_path(&file, &roots)
                .as_deref()
                .is_some_and(|path| scope.contains(path));
            if in_scope {
                manifest.files.push(file);
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
                remove_manifest_file(&mut manifest, &logical);
                tombstones.insert(
                    (logical.logical_root.as_token(), logical.relative_path),
                    "observed deletion or rename".into(),
                );
            }
        }
        if let Some(previous_path) = &mutation.previous_path {
            if let Some(logical) = roots.classify(Path::new(previous_path)) {
                remove_manifest_file(&mut manifest, &logical);
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
        let inherited = manifest_file(&manifest, &logical).cloned();
        remove_manifest_file(&mut manifest, &logical);
        if !Path::new(&canonical).is_file() {
            if manifest.parent_bundle_id.is_some() {
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
            && indexed.is_some_and(|state| {
                state.fast_identity_matches(metadata.len(), mtime_ns, inode, None)
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

    if include_paths.is_empty() && manifest.tombstones.is_empty() {
        if let Some(parent_bundle_id) = manifest.parent_bundle_id {
            if let Ok(parent_manifest) =
                read_committed_manifest(provider, master, app_id, parent_bundle_id).await
            {
                metrics.total_duration_ms =
                    elapsed_ms(total_started).saturating_add(metrics.discovery_duration_ms);
                finish_backup_evidence(agent, app_id, &pending_mutations)?;
                mark_noop_completed(agent, &mut op, &mut metrics, &parent_manifest)?;
                return Ok(parent_manifest);
            }
        }
    }

    persist_operation(agent, &mut op, BackupState::Snapshotting, &metrics)?;
    let snapshot_started = Instant::now();
    let view = create_view(&agent.config.paths.snapshots, &include_paths, true)?;
    let _snapshot_cleanup = SnapshotCleanup(view.root.clone());
    manifest.consistency = view.consistency;
    metrics.snapshot_duration_ms = elapsed_ms(snapshot_started);

    persist_operation(agent, &mut op, BackupState::Hashing, &metrics)?;
    progress.phase = "hashing".into();
    progress.completed_units = 0;
    agent.db.set_operation_progress(op_id, Some(&progress))?;
    let hashing_started = Instant::now();
    let cas = LocalCas::new(agent.config.paths.cache.join("cas/chunks"))?;
    let cancelled: CancellationCheck = {
        let operations = agent.operations.clone();
        Arc::new(move || operations.cancel_requested(op_id))
    };
    let snapshot_time = Utc::now();
    let storage_id = provider
        .storage_identity()
        .map(|identity| identity.cache_key());
    let remote_chunks = load_remote_chunk_index(agent, storage_id.as_deref(), snapshot_time)?;
    let cas_observed_at = snapshot_time;
    let mut cas_observations = Vec::<LocalCasEntry>::new();
    let mut progress_reporter = ProgressReporter::new(op_id);
    let inherited_hashes = pack_index
        .iter()
        .map(|entry| entry.chunk_hash.clone())
        .collect::<std::collections::HashSet<_>>();
    let jobs = view
        .mappings
        .iter()
        .filter_map(|mapping| {
            changed_files
                .get(&mapping.source.to_string_lossy().into_owned())
                .map(|(record, association, logical)| HashJob {
                    app_id: app_id.clone(),
                    source: mapping.source.clone(),
                    staged: mapping.staged.clone(),
                    record: record.clone(),
                    association: association.clone(),
                    logical: logical.clone(),
                    shared_app_ids: shared_app_ids_by_path
                        .get(&record.path_id)
                        .cloned()
                        .unwrap_or_default(),
                })
        })
        .collect::<Vec<_>>();
    let results = hash_files_parallel(
        jobs,
        hashing_worker_count(performance),
        cas,
        inherited_hashes,
        remote_chunks,
        cas_observed_at,
        Arc::clone(&cancelled),
    )?;
    let mut new_chunk_paths = BTreeMap::<String, PathBuf>::new();
    let mut small_file_chunk_hashes = BTreeSet::<String>::new();
    let mut trusted_states = Vec::<FileStateRecord>::new();
    for result in results {
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
        progress_reporter.maybe_flush(agent, &mut progress, &metrics)?;
    }
    progress_reporter.flush(agent, &mut progress, &metrics)?;
    metrics.hashing_duration_ms = elapsed_ms(hashing_started);

    persist_operation(agent, &mut op, BackupState::Packing, &metrics)?;
    progress.phase = "packing".into();
    agent.db.set_operation_progress(op_id, Some(&progress))?;
    let packing_started = Instant::now();
    let pack_dir = agent
        .config
        .paths
        .packs
        .join(manifest.bundle_id.to_string());
    let mut pack_cleanup = PackCleanup::new(pack_dir.clone());
    let mut small_chunks = Vec::new();
    let mut regular_chunks = Vec::new();
    for chunk in new_chunk_paths {
        if small_file_chunk_hashes.contains(&chunk.0) {
            small_chunks.push(chunk);
        } else {
            regular_chunks.push(chunk);
        }
    }
    // Small state files are deliberately grouped into compact packs so launch-critical
    // restore does not need to fetch a mostly unrelated 512 MiB pack.
    let mut packs = build_packs_parallel(
        &pack_dir,
        master,
        small_chunks,
        16 * 1024 * 1024,
        32 * 1024 * 1024,
        pack_worker_count(performance),
        Arc::clone(&cancelled),
    )?;
    packs.extend(build_packs_parallel(
        &pack_dir,
        master,
        regular_chunks,
        noland_state_core::constants::PACK_TARGET,
        noland_state_core::constants::PACK_MAX,
        pack_worker_count(performance),
        Arc::clone(&cancelled),
    )?);
    let mut incremental = 0u64;
    let mut pack_files = Vec::new();
    let mut new_pack_entries = Vec::<PackIndexEntry>::new();
    for pack in &packs {
        incremental = incremental.saturating_add(pack.bytes);
        metrics.bytes_packed = metrics.bytes_packed.saturating_add(pack.bytes);
        pack_files.push((pack.pack_id.clone(), pack.path.clone()));
        new_pack_entries.extend(pack.entries.iter().cloned());
    }
    pack_index.extend(new_pack_entries.iter().cloned());
    pack_index.sort_by(|left, right| left.chunk_hash.cmp(&right.chunk_hash));
    pack_index.dedup_by(|left, right| left.chunk_hash == right.chunk_hash);
    manifest.files.sort_by(|left, right| {
        (&left.logical_root, &left.relative_path).cmp(&(&right.logical_root, &right.relative_path))
    });
    manifest.tombstones.sort_by(|left, right| {
        (&left.logical_root, &left.relative_path).cmp(&(&right.logical_root, &right.relative_path))
    });
    noland_restore::embed_restore_plan(&mut manifest, restore_mode_for_backup(mode));
    metrics.packing_duration_ms = elapsed_ms(packing_started);

    persist_operation(agent, &mut op, BackupState::Uploading, &metrics)?;
    progress.phase = "uploading".into();
    progress.completed_units = 0;
    progress.total_units = Some(incremental);
    progress.unit = Some("bytes".into());
    progress.message = Some(format!("Uploading {incremental} bytes of changed state"));
    progress.detail_json = serde_json::json!({ "bytes_to_upload": incremental });
    progress.updated_at = Utc::now();
    agent.db.set_operation_progress(op_id, Some(&progress))?;
    let storage_before = provider.operation_metrics();
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
    pack_cleanup.mark_cloud_committed();
    metrics.upload_duration_ms = elapsed_ms(upload_started);
    metrics.num_manifest_writes = 1;

    persist_operation(agent, &mut op, BackupState::Committing, &metrics)?;
    let commit_started = Instant::now();
    update_catalog_with_bundle(provider, master, &manifest, incremental).await?;
    cache_parent(agent, &manifest, &pack_index)?;
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
    remember_remote_pack_index(agent, provider.storage_identity(), &pack_index)?;
    agent.db.upsert_local_cas_entries(&cas_observations)?;
    agent.db.upsert_file_states(&trusted_states)?;
    finish_backup_evidence(agent, app_id, &pending_mutations)?;

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
    discard(&view)?;

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
    Ok(manifest)
}

async fn load_parent(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    agent: &StateAgent,
    app_id: &AppId,
) -> Option<(BundleManifest, Vec<PackIndexEntry>)> {
    let (_, bundle_id, _) = agent.db.latest_commit(app_id).ok().flatten()?;
    if let Some(cached) = read_cached_parent(agent, app_id, bundle_id) {
        let _ = remember_remote_pack_index(agent, provider.storage_identity(), &cached.1);
        return Some(cached);
    }
    let manifest = read_committed_manifest(provider, master, app_id, bundle_id)
        .await
        .ok()?;
    let index = read_pack_index(provider, master, app_id, bundle_id)
        .await
        .ok()?;
    cache_parent(agent, &manifest, &index).ok()?;
    let _ = remember_remote_pack_index(agent, provider.storage_identity(), &index);
    Some((manifest, index))
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
) -> Option<(BundleManifest, Vec<PackIndexEntry>)> {
    let dir = parent_cache_dir(agent, app_id, bundle_id);
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
    index: &[PackIndexEntry],
) -> Result<()> {
    let Some(identity) = identity else {
        return Ok(());
    };
    let storage_id = identity.cache_key();
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
) -> Result<()> {
    let dir = parent_cache_dir(agent, &manifest.app.app_id, manifest.bundle_id);
    std::fs::create_dir_all(&dir)?;
    let manifest_temp = dir.join("manifest.json.tmp");
    let index_temp = dir.join("index.json.tmp");
    std::fs::write(&manifest_temp, serde_json::to_vec(manifest)?)?;
    std::fs::write(&index_temp, serde_json::to_vec(index)?)?;
    std::fs::rename(manifest_temp, dir.join("manifest.json"))?;
    std::fs::rename(index_temp, dir.join("index.json"))?;
    Ok(())
}

fn manifest_file<'a>(
    manifest: &'a BundleManifest,
    logical: &LogicalPath,
) -> Option<&'a ManifestFile> {
    manifest.files.iter().find(|file| {
        file.logical_root == logical.logical_root.as_token()
            && file.relative_path == logical.relative_path
    })
}

fn remove_manifest_file(manifest: &mut BundleManifest, logical: &LogicalPath) {
    manifest.files.retain(|file| {
        file.logical_root != logical.logical_root.as_token()
            || file.relative_path != logical.relative_path
    });
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
) -> Result<()> {
    let mutation_ids = mutations
        .iter()
        .map(|mutation| mutation.mutation_id)
        .collect::<Vec<_>>();
    agent
        .db
        .mark_app_mutations_processed(&mutation_ids, Utc::now())?;
    if !agent.db.pending_app_mutations(app_id, 1)?.is_empty() {
        return Ok(());
    }
    agent.db.clear_dirty_roots(app_id)?;
    agent.db.clear_dirty(app_id)
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
    let tuning = transfer_tuning(agent, performance);
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
    let tuning = transfer_tuning(agent, performance);
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

fn transfer_tuning(agent: &StateAgent, performance: BackupPerformanceMode) -> TransferTuning {
    if agent
        .db
        .open_session_app_ids()
        .is_ok_and(|sessions| !sessions.is_empty())
    {
        return TransferTuning::gameplay_safe();
    }
    match performance {
        BackupPerformanceMode::Fast | BackupPerformanceMode::Full => TransferTuning::throughput(),
        BackupPerformanceMode::Balanced => TransferTuning::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentConfig;
    use noland_storage::{read_pack_index, LocalStorage};

    fn test_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "noland-backup-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
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

        let manifest = run_backup_to_local(
            &agent,
            &app_id,
            BackupMode::CompleteApplication,
            cloud,
            &MasterKey::generate(),
        )
        .await
        .unwrap();
        assert!(manifest.files.is_empty());
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
        let root = std::env::temp_dir().join(format!(
            "noland-steam-manifest-tracking-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
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
        remember_remote_pack_index(&agent, storage.storage_identity(), &index).unwrap();

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
}
