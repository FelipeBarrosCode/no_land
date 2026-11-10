//! Ordered ciphertext -> authenticated records -> destination-side partial files.
//! Only final file data is retained: no full encrypted cache plus decrypted CAS.
//! Network pipes provide backpressure while bounded workers verify ready files.
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::future::{poll_fn, Future};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::task::Poll;
use std::time::{Duration, Instant};

use noland_crypto::MasterKey;
use noland_pack::{PackIndexEntry, PackStreamDecoder};
use noland_state_core::{
    constants::PACK_MAX, pack_key, ContentObjectKind, Result, StateError, SyncDirection,
    SyncJournalEntry,
};
use noland_state_db::StateDb;
use noland_storage::{DownloadObserver, DownloadStreamConsumer, RemoteKey, SharedStorageProvider};

use super::{
    destination_partial_path, journal, sync_parent_directory, validate_parent_chain,
    DownloadJournal, LogicalRootMap, PlannedDestination, PlannedTombstone, RestorePlan,
    RestorePriority, RestoreReport, RestoreTarget, RestoreTransaction,
};

const PUBLICATION_BATCH: usize = 128;

#[derive(Debug, Clone, Copy)]
pub struct PipelineOptions {
    pub max_parallel_packs: usize,
    /// Bounds encrypted scratch on non-streaming backends. Streaming uses pipes.
    pub max_inflight_pack_bytes: u64,
    pub reconstruction_workers: usize,
    /// Each authenticated record is at most FASTCDC_MAX bytes.
    pub buffered_chunks: usize,
}

impl Default for PipelineOptions {
    fn default() -> Self {
        Self {
            max_parallel_packs: 4,
            max_inflight_pack_bytes: PACK_MAX,
            reconstruction_workers: std::thread::available_parallelism()
                .map_or(1, usize::from)
                .min(4),
            buffered_chunks: 8,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct PipelineProgress {
    pub phase: String,
    pub checked_entries: u64,
    pub rollback_entries: u64,
    pub preflight_duration_ms: u64,
    pub rollback_duration_ms: u64,
    pub completed_entries: u64,
    pub reused_entries: u64,
    pub total_entries: u64,
    pub published_bytes: u64,
    pub assembled_bytes: u64,
    pub readback_bytes: u64,
    pub total_bytes: u64,
    pub ready_to_launch: bool,
    pub elapsed_ms: u64,
}

#[derive(Debug, Default)]
pub struct PipelineReport {
    pub restore: RestoreReport,
    pub apply_duration_ms: u64,
    pub pipeline_duration_ms: u64,
    pub packs_downloaded: usize,
}

struct PendingFile {
    planned: PlannedDestination,
    partial: PathBuf,
    remaining: usize,
    cursor: u64,
    received: u64,
    hasher: Option<Box<blake3::Hasher>>,
    out_of_order: bool,
    opened: bool,
    identity: Option<(u64, u64)>,
}

struct Placement {
    index: usize,
    offset: u64,
    length: u64,
}

struct Pipeline<'tx, 'plan, F> {
    transaction: &'tx mut RestoreTransaction<'plan>,
    pending: BTreeMap<usize, PendingFile>,
    consumers: BTreeMap<String, Vec<Placement>>,
    ready: Vec<PendingFile>,
    tombstones: Vec<PlannedTombstone>,
    critical: BTreeSet<usize>,
    progress: PipelineProgress,
    report: RestoreReport,
    workers: usize,
    on_progress: F,
    started: Instant,
    last_progress: Option<Instant>,
    apply_time: Duration,
    last_space_check: Instant,
}

fn poisoned() -> StateError {
    StateError::Storage("restore pipeline lock poisoned".into())
}

fn open_partial(pending: &PendingFile) -> Result<fs::File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .mode(0o600);
    if !pending.opened {
        options.create_new(true);
    }
    let file = options.open(&pending.partial)?;
    let meta = file.metadata()?;
    if !meta.is_file()
        || pending
            .identity
            .is_some_and(|identity| identity != (meta.dev(), meta.ino()))
    {
        return Err(StateError::UnsafePath(
            "restore partial identity changed".into(),
        ));
    }
    Ok(file)
}

fn verify_file(
    pending: &PendingFile,
    file: &noland_state_core::ManifestFile,
    readback: &AtomicU64,
    buffer: &mut [u8],
) -> Result<(u64, u64)> {
    validate_parent_chain(&pending.planned.root, &pending.partial)?;
    let mut output = open_partial(pending)?;
    let metadata = output.metadata()?;
    if pending.received != file.size || metadata.len() != file.size {
        return Err(StateError::Integrity(
            "reconstructed file size mismatch".into(),
        ));
    }
    let digest = if pending.out_of_order {
        let mut hasher = blake3::Hasher::new();
        loop {
            let count = output.read(buffer)?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
            readback.fetch_add(count as u64, Ordering::Relaxed);
        }
        format!("blake3:{}", hasher.finalize().to_hex())
    } else {
        format!(
            "blake3:{}",
            pending
                .hasher
                .as_ref()
                .map_or_else(
                    || blake3::Hasher::new().finalize(),
                    |hasher| hasher.finalize()
                )
                .to_hex()
        )
    };
    if digest != file.file_hash {
        return Err(StateError::Integrity(
            "reconstructed file hash mismatch".into(),
        ));
    }
    if let Some(mode) = file.mode {
        output.set_permissions(fs::Permissions::from_mode(mode))?;
    }
    output.sync_all()?;
    Ok((metadata.dev(), metadata.ino()))
}

impl<'tx, 'plan, F: FnMut(PipelineProgress) -> Result<()> + Send> Pipeline<'tx, 'plan, F> {
    fn new(
        transaction: &'tx mut RestoreTransaction<'plan>,
        workers: usize,
        mut on_progress: F,
    ) -> Result<Self> {
        let started = Instant::now();
        let mut initial = PipelineProgress {
            phase: "checking_existing_files".into(),
            total_entries: transaction.priorities.entries.len() as u64,
            ..PipelineProgress::default()
        };
        on_progress(initial.clone())?;
        let (planned, tombstones) =
            transaction.preflight_observed(RestoreTarget::Complete, |checked| {
                initial.checked_entries = checked;
                initial.elapsed_ms = started.elapsed().as_millis() as u64;
                on_progress(initial.clone())
            })?;
        initial.preflight_duration_ms = started.elapsed().as_millis() as u64;
        initial.phase = "preparing_rollback".into();
        initial.elapsed_ms = started.elapsed().as_millis() as u64;
        on_progress(initial.clone())?;
        let rollback_started = Instant::now();
        let mut last_rollback_progress = Instant::now();
        transaction.prepare_rollback_observed(&planned, &tombstones, |count| {
            initial.rollback_entries = count;
            if last_rollback_progress.elapsed() >= Duration::from_millis(500) {
                initial.elapsed_ms = started.elapsed().as_millis() as u64;
                initial.rollback_duration_ms = rollback_started.elapsed().as_millis() as u64;
                on_progress(initial.clone())?;
                last_rollback_progress = Instant::now();
            }
            Ok(())
        })?;
        let critical = transaction
            .priorities
            .entries
            .iter()
            .filter(|entry| entry.priority <= RestorePriority::Critical)
            .map(|entry| entry.manifest_index)
            .collect();
        let progress = PipelineProgress {
            phase: "preparing_destinations".into(),
            checked_entries: planned.len() as u64,
            rollback_entries: initial.rollback_entries,
            preflight_duration_ms: initial.preflight_duration_ms,
            rollback_duration_ms: rollback_started.elapsed().as_millis() as u64,
            total_entries: planned.len() as u64,
            total_bytes: planned
                .iter()
                .map(|entry| transaction.plan.manifest.files[entry.manifest_index].size)
                .sum(),
            ..PipelineProgress::default()
        };
        let mut this = Self {
            transaction,
            pending: BTreeMap::new(),
            consumers: BTreeMap::new(),
            ready: Vec::new(),
            tombstones,
            critical,
            progress,
            report: RestoreReport::default(),
            workers,
            on_progress,
            started,
            last_progress: None,
            apply_time: Duration::ZERO,
            last_space_check: Instant::now(),
        };

        // Parents exist and are durable before batching child intents. Recovery can
        // safely remove even a partial whose creation never happened after a crash.
        let mut intents = Vec::new();
        for entry in &planned {
            if entry.identical {
                continue;
            }
            let file = &this.transaction.plan.manifest.files[entry.manifest_index];
            let is_directory = file.file_type == "directory";
            let parent = if is_directory {
                entry.destination.as_path()
            } else {
                entry.destination.parent().ok_or_else(|| {
                    StateError::UnsafePath("restore destination has no parent".into())
                })?
            };
            this.transaction
                .create_directory_chain_journaled(&entry.root, parent)?;
            if !is_directory {
                let partial =
                    destination_partial_path(&entry.destination, this.transaction.plan.restore_id)?;
                this.transaction.active_partials.insert(partial.clone());
                intents.push(journal::Intent::Partial(partial));
                if !entry.existed {
                    this.transaction
                        .created_paths
                        .insert(entry.destination.clone());
                    intents.push(journal::Intent::Created(entry.destination.clone()));
                }
            }
            if intents.len() >= PUBLICATION_BATCH * 2 {
                journal::append(&this.transaction.plan.staging, &intents)?;
                intents.clear();
            }
            this.emit_progress(false)?;
        }
        journal::append(&this.transaction.plan.staging, &intents)?;
        for entry in planned {
            let index = entry.manifest_index;
            if entry.identical {
                this.mark_published(index, entry.destination, true)?;
                continue;
            }
            let file = &this.transaction.plan.manifest.files[index];
            match file.file_type.as_str() {
                "directory" => this.mark_published(index, entry.destination, false)?,
                "symlink" => {
                    this.transaction.publish_symlink(file, &entry)?;
                    this.mark_published(index, entry.destination, false)?;
                }
                "file" => {
                    let partial = destination_partial_path(
                        &entry.destination,
                        this.transaction.plan.restore_id,
                    )?;
                    let mut offset = 0u64;
                    for chunk in &file.chunks {
                        let length = chunk.size;
                        this.consumers
                            .entry(chunk.hash.clone())
                            .or_default()
                            .push(Placement {
                                index,
                                offset,
                                length,
                            });
                        offset = offset.checked_add(length).ok_or_else(|| {
                            StateError::Integrity("file chunk size overflow".into())
                        })?;
                    }
                    if offset != file.size {
                        return Err(StateError::Integrity(
                            "manifest chunk sizes do not match file".into(),
                        ));
                    }
                    let pending = PendingFile {
                        remaining: file.chunks.len(),
                        planned: entry,
                        partial,
                        cursor: 0,
                        received: 0,
                        hasher: None,
                        out_of_order: false,
                        opened: false,
                        identity: None,
                    };
                    if pending.remaining == 0 {
                        this.ready.push(pending);
                    } else {
                        this.pending.insert(index, pending);
                    }
                }
                _ => unreachable!("preflight validates file types"),
            }
        }
        this.flush_ready()?;
        this.progress.phase = "streaming".into();
        this.emit_progress(true)?;
        Ok(this)
    }

    fn mark_published(&mut self, index: usize, destination: PathBuf, reused: bool) -> Result<()> {
        self.transaction.release_entry_chunks(index)?;
        self.transaction.published_entries.insert(index);
        self.transaction.pending_associations.insert(index);
        self.critical.remove(&index);
        self.report.destinations.push(destination);
        if reused {
            self.report.reused_entries += 1;
            self.progress.reused_entries += 1;
        } else {
            self.report.published_entries += 1;
        }
        self.progress.completed_entries += 1;
        self.progress.published_bytes += self.transaction.plan.manifest.files[index].size;
        Ok(())
    }

    fn emit_progress(&mut self, force: bool) -> Result<()> {
        if force
            || self
                .last_progress
                .is_none_or(|last| last.elapsed() >= Duration::from_millis(500))
        {
            self.progress.ready_to_launch = self.critical.is_empty();
            self.progress.elapsed_ms =
                self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
            (self.on_progress)(self.progress.clone())?;
            self.last_progress = Some(Instant::now());
        }
        Ok(())
    }

    fn consume(&mut self, hash: &str, plain: &[u8]) -> Result<()> {
        // An rclone retry restarts the byte stream, not the restore. Each hash is
        // authenticated once per attempt but written to its destinations only once.
        let Some(placements) = self.consumers.remove(hash) else {
            return Ok(());
        };
        for placement in placements {
            let started = Instant::now();
            if placement.length != plain.len() as u64 {
                return Err(StateError::Integrity("chunk length mismatch".into()));
            }
            let pending = self
                .pending
                .get_mut(&placement.index)
                .ok_or_else(|| StateError::Integrity("missing pending restore file".into()))?;
            validate_parent_chain(&pending.planned.root, &pending.partial)?;
            if self.last_space_check.elapsed() >= Duration::from_secs(1) {
                noland_state_core::disk::ensure_space(&pending.partial, plain.len() as u64)?;
                self.last_space_check = Instant::now();
            }
            let mut output = open_partial(pending)?;
            let metadata = output.metadata()?;
            pending.identity = Some((metadata.dev(), metadata.ino()));
            pending.opened = true;
            output.seek(SeekFrom::Start(placement.offset))?;
            output.write_all(plain)?;
            // The normal ordered case hashes during writing. Out-of-order packs
            // use one bounded-worker readback, never an incorrectly ordered hash.
            if !pending.out_of_order && placement.offset == pending.cursor {
                pending
                    .hasher
                    .get_or_insert_with(|| Box::new(blake3::Hasher::new()))
                    .update(plain);
                pending.cursor += plain.len() as u64;
            } else {
                pending.out_of_order = true;
                pending.hasher = None;
            }
            pending.received += plain.len() as u64;
            pending.remaining -= 1;
            self.progress.assembled_bytes += plain.len() as u64;
            if pending.remaining == 0 {
                self.ready.push(
                    self.pending
                        .remove(&placement.index)
                        .expect("pending file exists"),
                );
            }
            self.apply_time += started.elapsed();
            if self.ready.len() >= PUBLICATION_BATCH {
                self.flush_ready()?;
            }
            self.emit_progress(false)?;
        }
        self.emit_progress(false)
    }

    fn flush_ready(&mut self) -> Result<()> {
        if self.ready.is_empty() {
            return Ok(());
        }
        let started = Instant::now();
        let ready = std::mem::take(&mut self.ready);
        let next = AtomicUsize::new(0);
        let manifest = &self.transaction.plan.manifest;
        let readback = AtomicU64::new(0);
        let readback_before = self.progress.readback_bytes;
        let progress = &mut self.progress;
        let on_progress = &mut self.on_progress;
        let last_progress = &mut self.last_progress;
        let pipeline_started = self.started;
        // Scoped workers cannot outlive cancellation/rollback or access the DB.
        let verified = std::thread::scope(|scope| -> Result<BTreeMap<usize, (u64, u64)>> {
            let mut handles = Vec::new();
            let (alive, finished) = std::sync::mpsc::channel::<()>();
            for _ in 0..self.workers.min(ready.len()) {
                let ready = &ready;
                let next = &next;
                let readback = &readback;
                let alive = alive.clone();
                handles.push(scope.spawn(move || -> Result<Vec<(usize, (u64, u64))>> {
                    let _alive = alive;
                    let mut verified = Vec::new();
                    let mut buffer = vec![0u8; 256 * 1024];
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(pending) = ready.get(index) else {
                            break;
                        };
                        let file = &manifest.files[pending.planned.manifest_index];
                        verified.push((index, verify_file(pending, file, readback, &mut buffer)?));
                    }
                    Ok(verified)
                }));
            }
            drop(alive);
            let mut first_error = None;
            while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) =
                finished.recv_timeout(Duration::from_millis(500))
            {
                progress.readback_bytes = readback_before + readback.load(Ordering::Relaxed);
                progress.elapsed_ms = pipeline_started
                    .elapsed()
                    .as_millis()
                    .min(u128::from(u64::MAX)) as u64;
                if first_error.is_none() {
                    if let Err(error) = on_progress(progress.clone()) {
                        first_error = Some(error);
                    }
                    *last_progress = Some(Instant::now());
                }
            }
            // Join EVERY worker, even after failure, before unwinding the restore.
            let mut verified = BTreeMap::new();
            for handle in handles {
                let result = handle.join().unwrap_or_else(|_| {
                    Err(StateError::Storage(
                        "restore verification worker panicked".into(),
                    ))
                });
                match result {
                    Ok(entries) => verified.extend(entries),
                    Err(error) => {
                        first_error.get_or_insert(error);
                    }
                }
            }
            progress.readback_bytes = readback_before + readback.load(Ordering::Relaxed);
            match first_error {
                Some(error) => Err(error),
                None => Ok(verified),
            }
        })?;
        let mut parents = BTreeSet::new();
        for (index, pending) in ready.iter().enumerate() {
            validate_parent_chain(&pending.planned.root, &pending.planned.destination)?;
            let metadata = fs::symlink_metadata(&pending.partial)?;
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || verified.get(&index) != Some(&(metadata.dev(), metadata.ino()))
            {
                return Err(StateError::UnsafePath(
                    "verified restore partial was replaced".into(),
                ));
            }
            fs::rename(&pending.partial, &pending.planned.destination)?;
            parents.insert(
                pending
                    .planned
                    .destination
                    .parent()
                    .expect("validated parent")
                    .to_path_buf(),
            );
        }
        for parent in parents {
            fs::File::open(parent)?.sync_all()?;
        }
        for pending in ready {
            self.transaction.clear_partial(&pending.partial)?;
            self.mark_published(
                pending.planned.manifest_index,
                pending.planned.destination,
                false,
            )?;
        }
        self.apply_time += started.elapsed();
        self.emit_progress(false)
    }

    fn finish(mut self) -> Result<PipelineReport> {
        self.flush_ready()?;
        if !self.pending.is_empty() || !self.consumers.is_empty() {
            return Err(StateError::Integrity(
                "restore ended before every file was reconstructed".into(),
            ));
        }
        for tombstone in &self.tombstones {
            if super::remove_path_if_present(&tombstone.destination)? {
                sync_parent_directory(&tombstone.destination)?;
            }
        }
        self.transaction.tombstones_applied = true;
        self.emit_progress(true)?;
        Ok(PipelineReport {
            restore: self.report,
            apply_duration_ms: self.apply_time.as_millis().min(u128::from(u64::MAX)) as u64,
            pipeline_duration_ms: self.started.elapsed().as_millis().min(u128::from(u64::MAX))
                as u64,
            packs_downloaded: 0,
        })
    }
}

struct PackJob {
    id: String,
    entries: Vec<PackIndexEntry>,
    size: u64,
    priority: RestorePriority,
}

fn pack_jobs(
    plan: &RestorePlan,
    index: &[PackIndexEntry],
    needed: &BTreeSet<String>,
) -> Result<Vec<PackJob>> {
    let mut by_hash = BTreeMap::new();
    let mut sizes = BTreeMap::<String, u64>::new();
    // One index build, not a full scan/hash of every local chunk at each tier.
    for entry in index {
        let end = entry
            .offset
            .checked_add(12 + entry.nonce.len() as u64 + u64::from(entry.ciphertext_len))
            .ok_or_else(|| StateError::Integrity("pack length overflow".into()))?;
        if end > PACK_MAX {
            return Err(StateError::Integrity("pack exceeds format limit".into()));
        }
        sizes
            .entry(entry.pack_id.clone())
            .and_modify(|size| *size = (*size).max(end))
            .or_insert(end);
        by_hash.entry(entry.chunk_hash.clone()).or_insert(entry);
    }
    let priorities = plan.priority_plan();
    let mut grouped = BTreeMap::<String, PackJob>::new();
    for planned in priorities.entries {
        for chunk in &plan.manifest.files[planned.manifest_index].chunks {
            if !needed.contains(&chunk.hash) {
                continue;
            }
            let entry = by_hash.get(&chunk.hash).ok_or_else(|| {
                StateError::Integrity("manifest chunk absent from pack index".into())
            })?;
            let job = grouped
                .entry(entry.pack_id.clone())
                .or_insert_with(|| PackJob {
                    id: entry.pack_id.clone(),
                    entries: Vec::new(),
                    size: sizes[&entry.pack_id],
                    priority: planned.priority,
                });
            job.priority = job.priority.min(planned.priority);
        }
    }
    for hash in needed {
        let entry = by_hash
            .get(hash)
            .ok_or_else(|| StateError::Integrity("missing indexed chunk".into()))?;
        grouped
            .get_mut(&entry.pack_id)
            .expect("needed pack grouped")
            .entries
            .push((*entry).clone());
    }
    let mut jobs = grouped.into_values().collect::<Vec<_>>();
    jobs.sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.id.cmp(&b.id)));
    Ok(jobs)
}

enum AssemblyMessage {
    Chunk(String, Vec<u8>),
    Flush,
    Finalize,
    Commit,
    Abort,
}

/// Dropping a cancelled future joins the worker *before* rollback may be observed.
/// It can never publish to destinations after the operation has terminated.
struct AssemblyGuard {
    thread: Option<std::thread::JoinHandle<()>>,
    sender: tokio::sync::mpsc::Sender<AssemblyMessage>,
    cancelled: Arc<AtomicBool>,
}

impl Drop for AssemblyGuard {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        let _ = self.sender.try_send(AssemblyMessage::Abort);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct StreamConsumer<'a> {
    job: &'a PackJob,
    master: &'a MasterKey,
    decoder: Mutex<PackStreamDecoder>,
    sender: &'a tokio::sync::mpsc::Sender<AssemblyMessage>,
}

#[async_trait::async_trait]
impl DownloadStreamConsumer for StreamConsumer<'_> {
    fn restart(&self) -> Result<()> {
        *self.decoder.lock().map_err(|_| poisoned())? =
            PackStreamDecoder::new(&self.job.id, self.job.entries.clone())?;
        Ok(())
    }
    async fn write(&self, mut bytes: &[u8]) -> Result<()> {
        while !bytes.is_empty() {
            let (consumed, record) = {
                let mut decoder = self.decoder.lock().map_err(|_| poisoned())?;
                let count = bytes.len().min(decoder.next_record_input_limit());
                let mut record = None;
                decoder.push(&bytes[..count], self.master, |entry, plain| {
                    record = Some((entry.chunk_hash.clone(), plain.to_vec()));
                    Ok(())
                })?;
                (count, record)
            };
            bytes = &bytes[consumed..];
            if let Some((hash, plain)) = record {
                self.sender
                    .send(AssemblyMessage::Chunk(hash, plain))
                    .await
                    .map_err(|_| StateError::Storage("restore assembly stopped".into()))?;
            }
        }
        Ok(())
    }
    async fn finish(&self) -> Result<()> {
        {
            let decoder = self.decoder.lock().map_err(|_| poisoned())?;
            decoder.finish()?;
            // Retained immutable packs can have unreferenced trailing records that
            // are omitted from this manifest's index; the indexed extent is a minimum.
            if decoder.received_bytes() < self.job.size {
                return Err(StateError::Integrity("streamed pack size mismatch".into()));
            }
        }
        self.sender
            .send(AssemblyMessage::Flush)
            .await
            .map_err(|_| StateError::Storage("restore assembly stopped".into()))
    }
}

struct PackObserver<'a> {
    journal: DownloadJournal<'a>,
    key: String,
}
impl DownloadObserver for PackObserver<'_> {
    fn transfer_progress(&self, bytes: u64) -> Result<()> {
        // Keep transfer bytes separate from the generic file publication counter.
        self.journal
            .db
            .update_sync_journal_progress(self.journal.operation_id, &self.key, bytes)
            .map(|_| ())
    }
}

struct ScratchGuard(PathBuf);
impl Drop for ScratchGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

async fn stream_pack(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    plan: &RestorePlan,
    job: PackJob,
    sender: &tokio::sync::mpsc::Sender<AssemblyMessage>,
    journal: Option<DownloadJournal<'_>>,
) -> Result<()> {
    let key = pack_key(&job.id);
    let scratch = ScratchGuard(
        plan.staging
            .join("packs")
            .join(format!("{}.stream.partial", job.id)),
    );
    let observer = journal.map(|journal| PackObserver {
        journal,
        key: key.clone(),
    });
    if let Some(journal) = journal {
        journal.db.start_sync_journal_item(
            journal.operation_id,
            &key,
            ContentObjectKind::Pack,
            SyncDirection::Download,
            Some(&scratch.0.to_string_lossy()),
            Some(&key),
            None,
        )?;
    }
    let consumer = StreamConsumer {
        decoder: Mutex::new(PackStreamDecoder::new(&job.id, job.entries.clone())?),
        job: &job,
        master,
        sender,
    };
    let result = provider
        .download_stream_observed(
            &RemoteKey::new(&key),
            &scratch.0,
            &consumer,
            observer
                .as_ref()
                .map(|observer| observer as &dyn DownloadObserver),
        )
        .await;
    if let Some(journal) = journal {
        match &result {
            Ok(()) => {
                let bytes = consumer
                    .decoder
                    .lock()
                    .map_err(|_| poisoned())?
                    .received_bytes();
                if let Some(mut entry) = journal
                    .db
                    .get_sync_journal_entry(journal.operation_id, &key)?
                {
                    entry.size = Some(bytes);
                    journal.db.upsert_sync_journal_entry(&entry)?;
                }
                journal
                    .db
                    .complete_sync_journal_item(journal.operation_id, &key, bytes)?;
            }
            Err(error) => {
                let _ = journal.db.fail_sync_journal_item(
                    journal.operation_id,
                    &key,
                    &error.to_string(),
                );
            }
        }
    }
    result
}

/// Rolling concurrency, additionally bounded in bytes. Completed slots refill
/// immediately instead of waiting for the slowest member of a download batch.
async fn run_weighted<T, Fut: Future<Output = Result<()>>>(
    jobs: Vec<(T, u64)>,
    limit: usize,
    budget: u64,
    mut operation: impl FnMut(T) -> Fut,
) -> Result<()> {
    let mut jobs = jobs.into_iter().peekable();
    let mut active: Vec<(u64, Pin<Box<Fut>>)> = Vec::new();
    let mut used = 0u64;
    loop {
        while active.len() < limit {
            let Some((_, size)) = jobs.peek() else {
                break;
            };
            if !active.is_empty() && used.saturating_add(*size) > budget {
                break;
            }
            let (job, size) = jobs.next().expect("peeked job");
            used += size;
            active.push((size, Box::pin(operation(job))));
        }
        if active.is_empty() {
            break;
        }
        let (index, result) = poll_fn(|cx| {
            for (index, (_, future)) in active.iter_mut().enumerate() {
                if let Poll::Ready(result) = future.as_mut().poll(cx) {
                    return Poll::Ready((index, result));
                }
            }
            Poll::Pending
        })
        .await;
        let (size, _) = active.swap_remove(index);
        used -= size;
        result?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn restore_pipelined(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    plan: &RestorePlan,
    index: &[PackIndexEntry],
    roots: &LogicalRootMap,
    db: Option<&StateDb>,
    journal: Option<DownloadJournal<'_>>,
    options: PipelineOptions,
    mut on_progress: impl FnMut(PipelineProgress) -> Result<()> + Send,
) -> Result<PipelineReport> {
    if options.max_parallel_packs == 0
        || options.max_inflight_pack_bytes < PACK_MAX
        || options.reconstruction_workers == 0
        || options.buffered_chunks == 0
        || options.buffered_chunks > 32
    {
        return Err(StateError::Invalid(
            "invalid bounded restore pipeline options".into(),
        ));
    }
    let (sender, mut receiver) = tokio::sync::mpsc::channel(options.buffered_chunks);
    let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel(32);
    let (init_tx, init_rx) = tokio::sync::oneshot::channel();
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_cancelled = Arc::clone(&cancelled);
    let worker_plan = plan.clone();
    let worker_roots = roots.clone();
    let worker_db = db.cloned();
    let thread = std::thread::spawn(move || {
        let result = (|| {
            let mut transaction =
                RestoreTransaction::new(&worker_plan, &worker_roots, worker_db.as_ref());
            transaction.uses_legacy_pack_cache = false;
            let mut pipeline = match Pipeline::new(
                &mut transaction,
                options.reconstruction_workers.min(16),
                |sample| {
                    if worker_cancelled.load(Ordering::Acquire) {
                        return Err(StateError::Storage("restore cancelled".into()));
                    }
                    let _ = progress_tx.try_send(sample);
                    Ok(())
                },
            ) {
                Ok(pipeline) => pipeline,
                Err(error) => {
                    let _ = init_tx.send(Err(StateError::Storage(error.to_string())));
                    return Err(error);
                }
            };
            let needed = pipeline.consumers.keys().cloned().collect::<BTreeSet<_>>();
            init_tx
                .send(Ok(needed))
                .map_err(|_| StateError::Storage("restore cancelled".into()))?;
            loop {
                if worker_cancelled.load(Ordering::Acquire) {
                    return Err(StateError::Storage("restore cancelled".into()));
                }
                match receiver.blocking_recv() {
                    Some(AssemblyMessage::Chunk(hash, plain)) => pipeline.consume(&hash, &plain)?,
                    Some(AssemblyMessage::Flush) => pipeline.flush_ready()?,
                    Some(AssemblyMessage::Finalize) => break,
                    Some(AssemblyMessage::Commit) => {
                        return Err(StateError::Invalid(
                            "restore commit before final validation".into(),
                        ))
                    }
                    Some(AssemblyMessage::Abort) | None => {
                        return Err(StateError::Storage("restore cancelled".into()))
                    }
                }
            }
            let report = pipeline.finish()?;
            ready_tx
                .send(())
                .map_err(|_| StateError::Storage("restore cancelled".into()))?;
            if !matches!(receiver.blocking_recv(), Some(AssemblyMessage::Commit)) {
                return Err(StateError::Storage(
                    "restore cancelled before commit".into(),
                ));
            }
            if worker_cancelled.load(Ordering::Acquire) {
                return Err(StateError::Storage("restore cancelled".into()));
            }
            transaction.commit()?;
            Ok(report)
        })();
        let _ = result_tx.send(result);
    });
    let _guard = AssemblyGuard {
        thread: Some(thread),
        sender: sender.clone(),
        cancelled,
    };
    let needed = monitor_progress(
        async {
            init_rx
                .await
                .map_err(|_| StateError::Storage("restore worker failed to initialize".into()))?
        },
        &mut progress_rx,
        &mut on_progress,
    )
    .await?;
    let jobs = pack_jobs(plan, index, &needed)?;
    let total_packs = jobs.len();
    if let Some(journal) = journal {
        for job in &jobs {
            let mut entry = SyncJournalEntry::pending(
                journal.operation_id,
                pack_key(&job.id),
                ContentObjectKind::Pack,
                SyncDirection::Download,
            );
            entry.size = Some(job.size);
            journal.db.upsert_sync_journal_entry(&entry)?;
        }
    }
    let transfer = run_weighted(
        jobs.into_iter()
            .map(|job| {
                let size = if provider.supports_streaming_downloads() {
                    // A pipe never retains a whole pack. Bound actual active records;
                    // the assembly channel separately bounds queued plaintext.
                    3 * noland_state_core::constants::FASTCDC_MAX + 1024
                } else {
                    PACK_MAX
                };
                (job, size)
            })
            .collect(),
        options.max_parallel_packs.min(32),
        options.max_inflight_pack_bytes,
        |job| stream_pack(provider, master, plan, job, &sender, journal),
    );
    let transfer_result = monitor_progress(transfer, &mut progress_rx, &mut on_progress).await;
    if transfer_result.is_err() {
        _guard.cancelled.store(true, Ordering::Release);
        let _ = sender.try_send(AssemblyMessage::Abort);
    } else {
        sender
            .send(AssemblyMessage::Finalize)
            .await
            .map_err(|_| StateError::Storage("restore assembly failed".into()))?;
        let ready = monitor_progress(
            async {
                ready_rx
                    .await
                    .map_err(|_| StateError::Storage("restore final validation failed".into()))
            },
            &mut progress_rx,
            &mut on_progress,
        )
        .await;
        if ready.is_ok() {
            while let Ok(sample) = progress_rx.try_recv() {
                on_progress(sample)?;
            }
            sender
                .send(AssemblyMessage::Commit)
                .await
                .map_err(|_| StateError::Storage("restore assembly stopped".into()))?;
        } else {
            _guard.cancelled.store(true, Ordering::Release);
            let _ = sender.try_send(AssemblyMessage::Abort);
        }
    }
    let assembly_result = monitor_progress(
        async {
            result_rx
                .await
                .map_err(|_| StateError::Storage("restore assembly worker panicked".into()))?
        },
        &mut progress_rx,
        &mut on_progress,
    )
    .await;
    // Prefer the assembly error (e.g. ENOSPC or a bad final hash) to a closed pipe.
    let mut report = assembly_result?;
    transfer_result?;
    while let Ok(sample) = progress_rx.try_recv() {
        on_progress(sample)?;
    }
    report.packs_downloaded = total_packs;
    Ok(report)
}

async fn monitor_progress<T>(
    future: impl Future<Output = Result<T>>,
    progress: &mut tokio::sync::mpsc::Receiver<PipelineProgress>,
    callback: &mut (impl FnMut(PipelineProgress) -> Result<()> + Send),
) -> Result<T> {
    tokio::pin!(future);
    loop {
        tokio::select! {
            result = &mut future => return result,
            Some(sample) = progress.recv() => callback(sample)?,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{block_on, crash_test_plan, test_dir, test_file, test_manifest, test_plan};
    use noland_state_core::{AgentPaths, ChunkRef, PersistenceClass, SemanticRole};
    use noland_storage::LocalStorage;

    #[test]
    fn direct_pipeline_handles_out_of_order_shared_chunks_without_a_decrypted_cas() {
        let root = test_dir("pipeline-scatter");
        let a = b"first";
        let b = b"second";
        let payload = [a.as_slice(), b.as_slice(), a.as_slice()].concat();
        let mut file = test_file(
            "game/save.dat",
            PersistenceClass::PersistentState,
            SemanticRole::UserState,
            &payload,
        );
        file.mode = Some(0o640);
        file.chunks = vec![
            ChunkRef {
                hash: noland_cas::blake3_hex(a),
                size: a.len() as u64,
            },
            ChunkRef {
                hash: noland_cas::blake3_hex(b),
                size: b.len() as u64,
            },
            ChunkRef {
                hash: noland_cas::blake3_hex(a),
                size: a.len() as u64,
            },
        ];
        let plan = test_plan(&root, test_manifest(vec![file]));
        let home = root.join("home");
        let roots = LogicalRootMap::from_home(&home);
        let destination = home.join(".local/share/game/save.dat");
        let mut transaction = RestoreTransaction::new(&plan, &roots, None);
        let mut samples = Vec::new();
        let mut pipeline = Pipeline::new(&mut transaction, 2, |sample| {
            samples.push(sample);
            Ok(())
        })
        .unwrap();
        pipeline.consume(&noland_cas::blake3_hex(a), a).unwrap();
        assert!(
            !destination.exists(),
            "an incomplete file cannot be published"
        );
        pipeline.consume(&noland_cas::blake3_hex(a), a).unwrap(); // retry must not write twice
        pipeline.consume(&noland_cas::blake3_hex(b), b).unwrap();
        let report = pipeline.finish().unwrap();
        transaction.commit().unwrap();
        assert_eq!(fs::read(&destination).unwrap(), payload);
        assert_eq!(
            fs::metadata(&destination).unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert_eq!(report.restore.published_entries, 1);
        assert!(samples.last().unwrap().ready_to_launch);
        assert!(samples.last().unwrap().readback_bytes > 0);
        assert!(samples
            .windows(2)
            .all(|pair| pair[0].completed_entries <= pair[1].completed_entries));
        assert!(!plan.staging.exists());
        assert!(!plan.pack_cache.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn streamed_restore_uses_existing_file_verification_and_no_tier_rescans() {
        let root = test_dir("pipeline-stream");
        let master = MasterKey::generate();
        let payload = b"latest-save";
        let file = test_file(
            "game/save.dat",
            PersistenceClass::PersistentState,
            SemanticRole::UserState,
            payload,
        );
        let packs = noland_pack::pack_chunks(
            &root.join("built"),
            &master,
            vec![(noland_cas::blake3_hex(payload), payload.to_vec())],
            |_| false,
        )
        .unwrap();
        let pack = &packs[0];
        let remote = root.join("remote");
        let remote_pack = remote.join(pack_key(&pack.pack_id));
        fs::create_dir_all(remote_pack.parent().unwrap()).unwrap();
        fs::copy(&pack.path, &remote_pack).unwrap();
        let storage = LocalStorage::new(&remote);
        let home = root.join("home");
        let roots = LogicalRootMap::from_home(&home);
        let first = test_plan(&root.join("first"), test_manifest(vec![file.clone()]));
        let report = block_on(restore_pipelined(
            &storage,
            &master,
            &first,
            &pack.entries,
            &roots,
            None,
            None,
            PipelineOptions::default(),
            |_| Ok(()),
        ))
        .unwrap();
        assert_eq!(report.packs_downloaded, 1);
        assert_eq!(
            fs::read(home.join(".local/share/game/save.dat")).unwrap(),
            payload
        );
        assert!(!first.pack_cache.exists());
        fs::remove_file(remote_pack).unwrap();
        let retry = test_plan(&root.join("retry"), test_manifest(vec![file]));
        let report = block_on(restore_pipelined(
            &storage,
            &master,
            &retry,
            &pack.entries,
            &roots,
            None,
            None,
            PipelineOptions::default(),
            |_| Ok(()),
        ))
        .unwrap();
        assert_eq!(report.packs_downloaded, 0);
        assert_eq!(report.restore.reused_entries, 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn asynchronous_publication_waits_for_progress_ack_and_rolls_back_on_cancellation() {
        let root = test_dir("pipeline-worker-cancel");
        let master = MasterKey::generate();
        let payload = b"new captured save";
        let file = test_file(
            "game/save.dat",
            PersistenceClass::PersistentState,
            SemanticRole::UserState,
            payload,
        );
        let packs = noland_pack::pack_chunks(
            &root.join("built"),
            &master,
            vec![(noland_cas::blake3_hex(payload), payload.to_vec())],
            |_| false,
        )
        .unwrap();
        let remote = root.join("remote");
        let remote_pack = remote.join(pack_key(&packs[0].pack_id));
        fs::create_dir_all(remote_pack.parent().unwrap()).unwrap();
        fs::copy(&packs[0].path, remote_pack).unwrap();
        let plan = test_plan(&root, test_manifest(vec![file]));
        let home = root.join("home");
        let destination = home.join(".local/share/game/save.dat");
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::write(&destination, b"original save").unwrap();
        let result = block_on(restore_pipelined(
            &LocalStorage::new(remote),
            &master,
            &plan,
            &packs[0].entries,
            &LogicalRootMap::from_home(home),
            None,
            None,
            PipelineOptions::default(),
            |sample| {
                if sample.completed_entries > 0 {
                    Err(StateError::Storage("cancel at publication boundary".into()))
                } else {
                    Ok(())
                }
            },
        ));
        assert!(result.is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"original save");
        assert!(
            !plan.staging.exists(),
            "worker must join and complete rollback before cancellation returns"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_final_hash_restores_original_save_and_removes_only_restore_paths() {
        let root = test_dir("pipeline-rollback");
        let file = test_file(
            "game/save.dat",
            PersistenceClass::PersistentState,
            SemanticRole::UserState,
            b"correct",
        );
        let plan = test_plan(&root, test_manifest(vec![file]));
        let home = root.join("home");
        let roots = LogicalRootMap::from_home(&home);
        let destination = home.join(".local/share/game/save.dat");
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::write(&destination, b"preserved-original").unwrap();
        {
            let mut transaction = RestoreTransaction::new(&plan, &roots, None);
            let mut pipeline = Pipeline::new(&mut transaction, 2, |_| Ok(())).unwrap();
            pipeline
                .consume(&noland_cas::blake3_hex(b"correct"), b"corrupt")
                .unwrap();
            assert!(pipeline.finish().is_err());
        }
        assert_eq!(fs::read(destination).unwrap(), b"preserved-original");
        assert!(!plan.staging.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn crash_after_pipelined_publication_recovers_using_durable_batched_intents() {
        let root = test_dir("pipeline-crash");
        let paths = AgentPaths::from_roots(root.join("state"), root.join("run"));
        paths.ensure_dirs().unwrap();
        let file = test_file(
            "new/deep/save.dat",
            PersistenceClass::PersistentState,
            SemanticRole::UserState,
            b"save",
        );
        let plan = crash_test_plan(&paths, test_manifest(vec![file]));
        let home = root.join("home");
        let roots = LogicalRootMap::from_home(&home);
        let destination = home.join(".local/share/new/deep/save.dat");
        let mut transaction = RestoreTransaction::new(&plan, &roots, None);
        let mut pipeline = Pipeline::new(&mut transaction, 2, |_| Ok(())).unwrap();
        pipeline
            .consume(&noland_cas::blake3_hex(b"save"), b"save")
            .unwrap();
        pipeline.finish().unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"save");
        std::mem::forget(transaction);
        let report = crate::recover_interrupted_restores(&paths).unwrap();
        assert_eq!(report.workspaces_rolled_back, 1);
        assert!(report.failures.is_empty());
        assert!(!destination.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn replacing_a_partial_with_a_symlink_cannot_modify_an_unrelated_file() {
        let root = test_dir("pipeline-partial-symlink");
        let mut file = test_file(
            "game/save.dat",
            PersistenceClass::PersistentState,
            SemanticRole::UserState,
            b"ab",
        );
        file.chunks = [b"a", b"b"]
            .into_iter()
            .map(|bytes| ChunkRef {
                hash: noland_cas::blake3_hex(bytes),
                size: 1,
            })
            .collect();
        let plan = test_plan(&root, test_manifest(vec![file]));
        let home = root.join("home");
        let roots = LogicalRootMap::from_home(&home);
        let unrelated = root.join("protected.dat");
        fs::write(&unrelated, b"do-not-touch").unwrap();
        {
            let mut transaction = RestoreTransaction::new(&plan, &roots, None);
            let mut pipeline = Pipeline::new(&mut transaction, 1, |_| Ok(())).unwrap();
            pipeline
                .consume(&noland_cas::blake3_hex(b"a"), b"a")
                .unwrap();
            let partial = pipeline.pending.values().next().unwrap().partial.clone();
            fs::remove_file(&partial).unwrap();
            std::os::unix::fs::symlink(&unrelated, partial).unwrap();
            assert!(pipeline
                .consume(&noland_cas::blake3_hex(b"b"), b"b")
                .is_err());
        }
        assert_eq!(fs::read(unrelated).unwrap(), b"do-not-touch");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rolling_download_pool_refills_before_slowest_download_finishes() {
        let started = AtomicUsize::new(0);
        block_on(run_weighted(vec![(0, 2), (1, 2), (2, 2)], 2, 4, |id| {
            let started = &started;
            async move {
                started.fetch_add(1, Ordering::Relaxed);
                let mut polls = 0;
                poll_fn(|cx| {
                    polls += 1;
                    assert!(
                        polls < 100,
                        "pool waited for the slowest batch member instead of refilling"
                    );
                    if id != 0 || started.load(Ordering::Relaxed) >= 3 {
                        Poll::Ready(Ok(()))
                    } else {
                        cx.waker().wake_by_ref();
                        Poll::Pending
                    }
                })
                .await
            }
        }))
        .unwrap();
        assert_eq!(started.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn crash_before_any_data_arrives_recovers_predeclared_partial_intents() {
        let root = test_dir("pipeline-crash-prepared");
        let paths = AgentPaths::from_roots(root.join("state"), root.join("run"));
        paths.ensure_dirs().unwrap();
        let file = test_file(
            "new/deep/save.dat",
            PersistenceClass::PersistentState,
            SemanticRole::UserState,
            b"save",
        );
        let plan = crash_test_plan(&paths, test_manifest(vec![file]));
        let roots = LogicalRootMap::from_home(root.join("home"));
        let mut transaction = RestoreTransaction::new(&plan, &roots, None);
        let pipeline = Pipeline::new(&mut transaction, 2, |_| Ok(())).unwrap();
        drop(pipeline);
        std::mem::forget(transaction);
        let report = crate::recover_interrupted_restores(&paths).unwrap();
        assert_eq!(report.workspaces_rolled_back, 1);
        assert!(report.failures.is_empty());
        assert!(!plan.staging.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn thousands_of_small_files_publish_without_per_file_journal_rewrites() {
        let root = test_dir("pipeline-small-files");
        let payload = b"deduplicated small file";
        let files = (0..4096)
            .map(|index| {
                test_file(
                    &format!("game/file-{index:05}"),
                    PersistenceClass::ReconstructableApp,
                    SemanticRole::AppContent,
                    payload,
                )
            })
            .collect();
        let plan = test_plan(&root, test_manifest(files));
        let roots = LogicalRootMap::from_home(root.join("home"));
        let mut transaction = RestoreTransaction::new(&plan, &roots, None);
        let mut updates = 0;
        let mut pipeline = Pipeline::new(&mut transaction, 4, |_| {
            updates += 1;
            Ok(())
        })
        .unwrap();
        let checkpoint =
            fs::read(plan.staging.join(super::super::TRANSACTION_JOURNAL_FILE)).unwrap();
        let log_size = fs::metadata(plan.staging.join("restore-intents.v1.log"))
            .unwrap()
            .len();
        pipeline
            .consume(&noland_cas::blake3_hex(payload), payload)
            .unwrap();
        assert_eq!(pipeline.progress.completed_entries, 4096);
        assert_eq!(
            fs::metadata(plan.staging.join("restore-intents.v1.log"))
                .unwrap()
                .len(),
            log_size
        );
        assert_eq!(
            fs::read(plan.staging.join(super::super::TRANSACTION_JOURNAL_FILE)).unwrap(),
            checkpoint
        );
        assert_eq!(
            fs::read_dir(plan.staging.join("materialized/.chunks"))
                .unwrap()
                .count(),
            0
        );
        pipeline.finish().unwrap();
        transaction.commit().unwrap();
        assert!(updates >= 2);
        fs::remove_dir_all(root).unwrap();
    }
}
