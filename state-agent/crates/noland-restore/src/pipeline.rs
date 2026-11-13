//! Ordered ciphertext -> authenticated records -> destination-side partial files.
//! Only final file data is retained: no full encrypted cache plus decrypted CAS.
//! Network pipes provide backpressure while persistent workers verify and flush
//! completed files concurrently with assembly of a bounded next window.
mod decode;
#[cfg(test)]
mod locality_tests;
#[cfg(test)]
mod publication_tests;
mod verification;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, OpenOptions};
use std::future::{poll_fn, Future};
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::task::Poll;
use std::time::{Duration, Instant};

use decode::{DecodeStats, StreamConsumer};
use noland_crypto::MasterKey;
use noland_pack::PackIndexEntry;
use noland_state_core::{
    constants::PACK_MAX, pack_key, ContentObjectKind, Result, StateError, SyncDirection,
    SyncJournalEntry,
};
use noland_state_db::StateDb;
#[cfg(test)]
use noland_storage::DownloadStreamConsumer;
use noland_storage::{
    DownloadObserver, RemoteKey, SharedStorageProvider, StreamTransferDiagnostics,
};
#[cfg(test)]
use std::sync::Mutex;

use super::identity::VerifiedIdentity;
use verification::{VerificationPool, VerificationTask, VerifiedFile};

use super::{
    destination_partial_path, journal, validate_parent_chain, DownloadJournal, LogicalRootMap,
    PlannedDestination, PlannedTombstone, RestorePlan, RestorePriority, RestorePriorityPlan,
    RestoreReport, RestoreTarget, RestoreTransaction, SharedRestorePlan,
};

const PUBLICATION_BATCH: usize = 128;
const PUBLICATION_BYTES: u64 = 32 * 1024 * 1024;
const PUBLICATION_DELAY: Duration = Duration::from_millis(100);
// Two windows include queued, verifying and verified-but-unpublished files.
// One oversized file is admitted alone; no file contents are buffered here.
const MAX_UNPUBLISHED_FILES: usize = PUBLICATION_BATCH * 2;
const MAX_UNPUBLISHED_BYTES: u64 = PUBLICATION_BYTES * 2;
// Independent of the number of pending files and active downloads. Completed
// files release their assembly handle before admission to verification.
const MAX_ASSEMBLY_HANDLES: usize = 32;
// Shared first chunks can leave arbitrarily many files incomplete. Hash state
// has its own budget; eviction forces mandatory full-file readback verification.
const MAX_ACTIVE_HASHERS: usize = 128;

pub(super) fn elapsed_us(started: Instant) -> u64 {
    started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}

#[derive(Debug, Clone, Copy)]
pub struct PipelineOptions {
    pub max_parallel_packs: usize,
    /// Bounds encrypted scratch on non-streaming backends. Streaming uses pipes.
    pub max_inflight_pack_bytes: u64,
    pub reconstruction_workers: usize,
    /// Overlap assembly with the previous window's file flushes. Enabled for
    /// Linux cloud agents. APFS serializes small writes behind concurrent fsyncs,
    /// so other hosts retain staged writes while reusing the same worker pool.
    pub overlap_file_syncs: bool,
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
            overlap_file_syncs: cfg!(target_os = "linux"),
            buffered_chunks: 8,
        }
    }
}

/// Cumulative I/O timings, in microseconds. Worker times are summed across
/// workers and overlap assembly, so they must not be added to elapsed time.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct PipelineIoMetrics {
    pub overlap_file_syncs: bool,
    pub write_us: u64,
    pub inline_hash_us: u64,
    pub verify_us: u64,
    pub file_sync_us: u64,
    pub rename_us: u64,
    pub directory_sync_us: u64,
    /// Batched file-intent appends; directory intents are in destination_prepare_us.
    pub intent_journal_us: u64,
    pub destination_prepare_us: u64,
    pub publication_wait_us: u64,
    pub file_syncs: u64,
    pub directory_syncs: u64,
    pub publication_batches: u64,
    pub peak_unpublished_files: u64,
    pub peak_unpublished_bytes: u64,
    pub assembly_opens: u64,
    pub assembly_handle_hits: u64,
    pub assembly_handle_evictions: u64,
    pub peak_assembly_handles: u64,
    pub out_of_order_files: u64,
    pub sequential_placements: u64,
    pub assembly_queue_wait_us: u64,
    pub pack_decode_us: u64,
    pub decode_dispatch_wait_us: u64,
    pub peak_decode_workers: u64,
    pub peak_active_hashers: u64,
    pub hash_state_evictions: u64,
    pub commit: super::RestoreCommitMetrics,
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
    /// Newly reconstructed bytes that passed full-file verification and sync.
    pub verified_bytes: u64,
    pub readback_bytes: u64,
    pub total_bytes: u64,
    pub ready_to_launch: bool,
    pub elapsed_ms: u64,
    pub io: PipelineIoMetrics,
}

#[derive(Debug, Default)]
pub struct PipelineReport {
    pub restore: RestoreReport,
    pub apply_duration_ms: u64,
    pub pipeline_duration_ms: u64,
    pub packs_downloaded: usize,
    pub io: PipelineIoMetrics,
}

struct PendingFile {
    planned: PlannedDestination,
    partial: PathBuf,
    remaining: usize,
    cursor: u64,
    received: u64,
    hasher: Option<Box<blake3::Hasher>>,
    out_of_order: bool,
    readback: bool,
    opened: bool,
    identity: Option<(u64, u64)>,
    last_write: Option<VerifiedIdentity>,
}

#[derive(Default)]
struct AssemblyHandles {
    files: VecDeque<(usize, fs::File)>,
}

impl AssemblyHandles {
    fn get<'a>(
        &'a mut self,
        pending: &PendingFile,
        metrics: &mut PipelineIoMetrics,
    ) -> Result<(&'a mut fs::File, VerifiedIdentity)> {
        // Recheck on EVERY use, including cache hits. A pinned descriptor cannot
        // redirect a write; a replaced path or parent is rejected rather than
        // silently writing an unlinked file and later publishing its replacement.
        validate_parent_chain(&pending.planned.root, &pending.partial)?;
        let index = pending.planned.manifest_index;
        let metadata = if let Some(position) = self.files.iter().position(|(key, _)| *key == index)
        {
            let entry = self.files.remove(position).expect("cached handle");
            let metadata = fs::symlink_metadata(&pending.partial)?;
            if !metadata.is_file()
                || metadata.nlink() != 1
                || pending.identity != Some((metadata.dev(), metadata.ino()))
            {
                return Err(StateError::UnsafePath(
                    "restore partial identity changed".into(),
                ));
            }
            self.files.push_back(entry);
            metrics.assembly_handle_hits += 1;
            self.files.back().expect("cached handle").1.metadata()?
        } else {
            if self.files.len() == MAX_ASSEMBLY_HANDLES {
                self.files.pop_front();
                metrics.assembly_handle_evictions += 1;
            }
            let (file, metadata) = open_partial(pending)?;
            self.files.push_back((index, file));
            metrics.assembly_opens += 1;
            metrics.peak_assembly_handles =
                metrics.peak_assembly_handles.max(self.files.len() as u64);
            metadata
        };
        let file = &mut self.files.back_mut().expect("opened handle").1;
        let identity = VerifiedIdentity::from_metadata(&metadata);
        if metadata.nlink() != 1
            || pending
                .last_write
                .is_some_and(|previous| previous != identity)
        {
            return Err(StateError::Integrity(
                "restore partial changed between writes".into(),
            ));
        }
        Ok((file, identity))
    }

    fn remove(&mut self, index: usize) {
        if let Some(position) = self.files.iter().position(|(key, _)| *key == index) {
            self.files.remove(position);
        }
    }
}

struct Placement {
    index: usize,
    offset: u64,
    length: u64,
}

struct Pipeline<'tx, 'plan, F> {
    transaction: &'tx mut RestoreTransaction<'plan>,
    pending: BTreeMap<usize, PendingFile>,
    handles: AssemblyHandles,
    active_hashers: VecDeque<usize>,
    consumers: BTreeMap<String, Vec<Placement>>,
    assembled: Vec<PendingFile>,
    assembled_bytes: u64,
    assembled_since: Option<Instant>,
    ready: Vec<VerifiedFile>,
    ready_bytes: u64,
    verifying_files: usize,
    unpublished_files: usize,
    unpublished_bytes: u64,
    verification: VerificationPool,
    overlap_file_syncs: bool,
    tombstones: Vec<PlannedTombstone>,
    critical: BTreeSet<usize>,
    progress: PipelineProgress,
    report: RestoreReport,
    on_progress: F,
    started: Instant,
    last_progress: Option<Instant>,
    apply_time: Duration,
    last_space_check: Instant,
    queue_wait_us: Arc<AtomicU64>,
    decode_stats: Arc<DecodeStats>,
}

fn open_partial(pending: &PendingFile) -> Result<(fs::File, fs::Metadata)> {
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
        || meta.nlink() != 1
        || pending
            .identity
            .is_some_and(|identity| identity != (meta.dev(), meta.ino()))
    {
        return Err(StateError::UnsafePath(
            "restore partial identity changed".into(),
        ));
    }
    Ok((file, meta))
}

impl<'tx, 'plan, F: FnMut(PipelineProgress) -> Result<()> + Send> Pipeline<'tx, 'plan, F> {
    fn new(
        transaction: &'tx mut RestoreTransaction<'plan>,
        workers: usize,
        mut on_progress: F,
    ) -> Result<Self> {
        transaction.begin_index_observation()?;
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
            handles: AssemblyHandles::default(),
            active_hashers: VecDeque::new(),
            consumers: BTreeMap::new(),
            assembled: Vec::new(),
            assembled_bytes: 0,
            assembled_since: None,
            ready: Vec::new(),
            ready_bytes: 0,
            verifying_files: 0,
            unpublished_files: 0,
            unpublished_bytes: 0,
            verification: VerificationPool::new(workers)?,
            overlap_file_syncs: PipelineOptions::default().overlap_file_syncs,
            tombstones,
            critical,
            progress,
            report: RestoreReport::default(),
            on_progress,
            started,
            last_progress: None,
            apply_time: Duration::ZERO,
            last_space_check: Instant::now(),
            queue_wait_us: Arc::new(AtomicU64::new(0)),
            decode_stats: Arc::default(),
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
            let prepare_started = Instant::now();
            this.transaction
                .create_directory_chain_journaled(&entry.root, parent)?;
            this.progress.io.destination_prepare_us += elapsed_us(prepare_started);
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
                let journal_started = Instant::now();
                journal::append(&this.transaction.plan.staging, &intents)?;
                this.progress.io.intent_journal_us += elapsed_us(journal_started);
                intents.clear();
            }
            this.emit_progress(false)?;
        }
        let journal_started = Instant::now();
        journal::append(&this.transaction.plan.staging, &intents)?;
        this.progress.io.intent_journal_us += elapsed_us(journal_started);
        for entry in planned {
            let index = entry.manifest_index;
            if entry.identical {
                if let Some(identity) = entry.verified {
                    this.transaction.verified_files.insert(index, identity);
                }
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
                        readback: false,
                        opened: false,
                        identity: None,
                        last_write: None,
                    };
                    if pending.remaining == 0 {
                        this.queue_completed(pending)?;
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
            self.verification.snapshot(&mut self.progress);
            self.progress.io.overlap_file_syncs = self.overlap_file_syncs;
            self.progress.io.assembly_queue_wait_us = self.queue_wait_us.load(Ordering::Relaxed);
            self.decode_stats.snapshot(&mut self.progress.io);
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
            if !self.overlap_file_syncs && self.verifying_files != 0 {
                self.wait_for_publication()?;
            }
            let started = Instant::now();
            if placement.length != plain.len() as u64 {
                return Err(StateError::Integrity("chunk length mismatch".into()));
            }
            self.admit_hasher(placement.index, placement.offset)?;
            let pending = self
                .pending
                .get_mut(&placement.index)
                .ok_or_else(|| StateError::Integrity("missing pending restore file".into()))?;
            if self.last_space_check.elapsed() >= Duration::from_secs(1) {
                noland_state_core::disk::ensure_space(&pending.partial, plain.len() as u64)?;
                self.last_space_check = Instant::now();
            }
            let write_started = Instant::now();
            let (output, identity) = self.handles.get(pending, &mut self.progress.io)?;
            pending.identity = Some((identity.device, identity.inode));
            pending.opened = true;
            output.seek(SeekFrom::Start(placement.offset))?;
            output.write_all(plain)?;
            pending.last_write = Some(VerifiedIdentity::from_metadata(&output.metadata()?));
            self.progress.io.write_us += elapsed_us(write_started);
            // The normal ordered case hashes during writing. Out-of-order packs
            // use one bounded-worker readback, never an incorrectly ordered hash.
            let hash_started = Instant::now();
            if !pending.out_of_order && placement.offset == pending.cursor {
                self.progress.io.sequential_placements += 1;
                if let Some(hasher) = &mut pending.hasher {
                    hasher.update(plain);
                }
                pending.cursor += plain.len() as u64;
            } else {
                if !pending.out_of_order {
                    self.progress.io.out_of_order_files += 1;
                }
                pending.out_of_order = true;
                pending.readback = true;
                pending.hasher = None;
            }
            self.progress.io.inline_hash_us += elapsed_us(hash_started);
            pending.received += plain.len() as u64;
            pending.remaining -= 1;
            self.progress.assembled_bytes += plain.len() as u64;
            let completed = pending.remaining == 0;
            let release_hasher = completed || pending.readback;
            self.apply_time += started.elapsed();
            if release_hasher {
                self.active_hashers
                    .retain(|&index| index != placement.index);
            }
            if completed {
                self.handles.remove(placement.index);
                let pending = self
                    .pending
                    .remove(&placement.index)
                    .expect("pending file exists");
                self.queue_completed(pending)?;
            }
            self.tick()?;
        }
        self.emit_progress(false)
    }

    fn admit_hasher(&mut self, index: usize, offset: u64) -> Result<()> {
        let pending = self
            .pending
            .get(&index)
            .ok_or_else(|| StateError::Integrity("missing pending restore file".into()))?;
        if pending.readback || pending.hasher.is_some() || offset != pending.cursor {
            return Ok(());
        }
        if self.active_hashers.len() == MAX_ACTIVE_HASHERS {
            let evicted = self.active_hashers.pop_front().expect("full hash budget");
            let pending = self
                .pending
                .get_mut(&evicted)
                .expect("active incomplete hasher");
            pending.hasher = None;
            pending.readback = true;
            self.progress.io.hash_state_evictions += 1;
        }
        self.pending.get_mut(&index).expect("pending file").hasher =
            Some(Box::new(blake3::Hasher::new()));
        self.active_hashers.push_back(index);
        self.progress.io.peak_active_hashers = self
            .progress
            .io
            .peak_active_hashers
            .max(self.active_hashers.len() as u64);
        Ok(())
    }

    fn queue_completed(&mut self, pending: PendingFile) -> Result<()> {
        let size = pending.received;
        while self.unpublished_files != 0
            && (self.unpublished_files >= MAX_UNPUBLISHED_FILES
                || self.unpublished_bytes.saturating_add(size) > MAX_UNPUBLISHED_BYTES)
        {
            self.wait_for_publication()?;
        }
        self.assembled_since.get_or_insert_with(Instant::now);
        self.assembled_bytes += size;
        self.assembled.push(pending);
        self.unpublished_files += 1;
        self.unpublished_bytes += size;
        self.progress.io.peak_unpublished_files = self
            .progress
            .io
            .peak_unpublished_files
            .max(self.unpublished_files as u64);
        self.progress.io.peak_unpublished_bytes = self
            .progress
            .io
            .peak_unpublished_bytes
            .max(self.unpublished_bytes);
        self.dispatch_if_due()
    }

    fn dispatch_if_due(&mut self) -> Result<()> {
        if self.assembled.len() >= PUBLICATION_BATCH
            || self.assembled_bytes >= PUBLICATION_BYTES
            || self
                .assembled_since
                .is_some_and(|since| since.elapsed() >= PUBLICATION_DELAY)
        {
            self.dispatch_assembled()?;
        }
        Ok(())
    }

    fn dispatch_assembled(&mut self) -> Result<()> {
        if self.verifying_files != 0 {
            return Ok(());
        }
        // Give the filesystem a window of small writes before starting flushes.
        // Eager per-file dispatch causes write/fsync contention on journaled disks.
        self.verifying_files = self.assembled.len();
        for pending in std::mem::take(&mut self.assembled) {
            let file = &self.transaction.plan.manifest.files[pending.planned.manifest_index];
            self.verification
                .submit(VerificationTask::new(pending, file))?;
        }
        self.assembled_bytes = 0;
        self.assembled_since = None;
        Ok(())
    }

    fn collect_verified(&mut self, mut timeout: Duration) -> Result<()> {
        while self.ready.len() < self.verifying_files {
            let Some(verified) = self.verification.receive(timeout)? else {
                break;
            };
            self.ready_bytes += verified.pending.received;
            self.ready.push(verified);
            timeout = Duration::ZERO;
        }
        Ok(())
    }

    fn publication_due(&self) -> bool {
        // Let the active flush window finish before renaming its files. Starting
        // another fsync window during publication serializes tiny rename writes
        // behind journal commits on APFS/ext4 and defeats batching.
        self.verifying_files != 0 && self.ready.len() == self.verifying_files
    }

    // Called for incoming data AND idle receive timeouts. A slow pack must not
    // keep a small completed batch unpublished indefinitely.
    fn tick(&mut self) -> Result<()> {
        self.collect_verified(Duration::ZERO)?;
        if self.publication_due() {
            self.publish_ready()?;
        }
        self.dispatch_if_due()?;
        self.emit_progress(false)
    }

    fn wait_for_publication(&mut self) -> Result<()> {
        self.dispatch_assembled()?;
        let started = Instant::now();
        loop {
            self.collect_verified(PUBLICATION_DELAY)?;
            if self.publication_due() {
                break;
            }
            self.emit_progress(false)?;
        }
        self.progress.io.publication_wait_us += elapsed_us(started);
        self.apply_time += started.elapsed();
        self.publish_ready()
    }

    fn flush_ready(&mut self) -> Result<()> {
        while self.unpublished_files != 0 {
            self.wait_for_publication()?;
        }
        Ok(())
    }

    fn publish_ready(&mut self) -> Result<()> {
        if self.ready.is_empty() {
            return Ok(());
        }
        let started = Instant::now();
        let mut ready = std::mem::take(&mut self.ready);
        let mut parents = BTreeSet::new();
        let rename_started = Instant::now();
        for verified in &mut ready {
            let pending = &verified.pending;
            validate_parent_chain(&pending.planned.root, &pending.planned.destination)?;
            let metadata = fs::symlink_metadata(&pending.partial)?;
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || verified.identity != VerifiedIdentity::from_metadata(&metadata)
            {
                return Err(StateError::UnsafePath(
                    "verified restore partial was replaced".into(),
                ));
            }
            fs::rename(&pending.partial, &pending.planned.destination)?;
            let published = fs::symlink_metadata(&pending.planned.destination)?;
            let identity = VerifiedIdentity::from_metadata(&published);
            if !published.is_file() || !verified.identity.matches_after_rename(identity) {
                return Err(StateError::Integrity(
                    "restored file changed during publication".into(),
                ));
            }
            // Only rename may change ctime. Retain that immediate observation,
            // rather than adopting edits made during the remaining batch/fsyncs.
            verified.identity = identity;
            parents.insert(
                pending
                    .planned
                    .destination
                    .parent()
                    .expect("validated parent")
                    .to_path_buf(),
            );
        }
        self.progress.io.rename_us += elapsed_us(rename_started);
        let directory_started = Instant::now();
        // Coalesce parents across the entire verified window before acknowledging
        // any publication. The next window remains write-only until this barrier.
        for parent in parents {
            fs::File::open(parent)?.sync_all()?;
            self.progress.io.directory_syncs += 1;
        }
        self.progress.io.directory_sync_us += elapsed_us(directory_started);
        self.progress.io.publication_batches += 1;
        self.unpublished_files -= ready.len();
        self.unpublished_bytes -= self.ready_bytes;
        self.ready_bytes = 0;
        self.verifying_files = 0;
        for verified in ready {
            let pending = verified.pending;
            let metadata = fs::symlink_metadata(&pending.planned.destination)?;
            let identity = VerifiedIdentity::from_metadata(&metadata);
            if !metadata.is_file() || verified.identity != identity {
                return Err(StateError::Integrity(
                    "restored file changed during publication".into(),
                ));
            }
            self.transaction
                .verified_files
                .insert(pending.planned.manifest_index, identity);
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
            tombstone.apply()?;
        }
        self.transaction.tombstones_applied = true;
        self.emit_progress(true)?;
        Ok(PipelineReport {
            restore: self.report,
            apply_duration_ms: self.apply_time.as_millis().min(u128::from(u64::MAX)) as u64,
            pipeline_duration_ms: self.started.elapsed().as_millis().min(u128::from(u64::MAX))
                as u64,
            packs_downloaded: 0,
            io: self.progress.io,
        })
    }
}

struct PackJob {
    id: String,
    entries: Vec<PackIndexEntry>,
    size: u64,
    priority: RestorePriority,
    first_use: (usize, usize),
    selected_chunk_references: u64,
    selected_plaintext_bytes: u64,
}

fn pack_jobs(
    plan: &RestorePlan,
    priorities: &RestorePriorityPlan,
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
        by_hash.entry(entry.chunk_hash.as_str()).or_insert(entry);
    }
    let mut grouped = BTreeMap::<String, PackJob>::new();
    for planned in &priorities.entries {
        for (chunk_index, chunk) in plan.manifest.files[planned.manifest_index]
            .chunks
            .iter()
            .enumerate()
        {
            if !needed.contains(&chunk.hash) {
                continue;
            }
            let entry = by_hash.get(chunk.hash.as_str()).ok_or_else(|| {
                StateError::Integrity("manifest chunk absent from pack index".into())
            })?;
            let job = grouped
                .entry(entry.pack_id.clone())
                .or_insert_with(|| PackJob {
                    id: entry.pack_id.clone(),
                    entries: Vec::new(),
                    size: sizes[&entry.pack_id],
                    priority: planned.priority,
                    first_use: (planned.manifest_index, chunk_index),
                    selected_chunk_references: 0,
                    selected_plaintext_bytes: 0,
                });
            job.selected_chunk_references = job.selected_chunk_references.saturating_add(1);
            job.selected_plaintext_bytes = job.selected_plaintext_bytes.saturating_add(chunk.size);
            let first_use = (planned.manifest_index, chunk_index);
            if (planned.priority, first_use) < (job.priority, job.first_use) {
                job.priority = planned.priority;
                job.first_use = first_use;
            }
        }
    }
    for hash in needed {
        let entry = by_hash
            .get(hash.as_str())
            .ok_or_else(|| StateError::Integrity("missing indexed chunk".into()))?;
        grouped
            .get_mut(&entry.pack_id)
            .expect("needed pack grouped")
            .entries
            .push((*entry).clone());
    }
    let mut jobs = grouped.into_values().collect::<Vec<_>>();
    // Capture packs follow manifest/file/chunk order, whereas their UUIDs are
    // random. The earliest use in the highest-priority tier preserves locality
    // without a barrier between downloads or extra plaintext buffering.
    jobs.sort_by_key(|job| (job.priority, job.first_use));
    for job in &mut jobs {
        job.entries.sort_by_key(|entry| entry.offset);
    }
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

struct PackObserver<'a> {
    journal: DownloadJournal<'a>,
    key: String,
}
impl DownloadObserver for PackObserver<'_> {
    fn transfer_started(&self) -> Result<()> {
        self.journal
            .db
            .set_sync_journal_state(
                self.journal.operation_id,
                &self.key,
                noland_state_core::SyncJournalState::InProgress,
                None,
                None,
            )
            .map(|_| ())
    }

    fn transfer_retrying(&self, reason: &str) -> Result<()> {
        self.journal
            .db
            .set_sync_journal_state(
                self.journal.operation_id,
                &self.key,
                noland_state_core::SyncJournalState::RetryScheduled,
                Some(reason),
                None,
            )
            .map(|_| ())
    }

    fn transfer_progress(&self, bytes: u64) -> Result<()> {
        // Keep transfer bytes separate from the generic file publication counter.
        self.journal
            .db
            .update_sync_journal_progress(self.journal.operation_id, &self.key, bytes)
            .map(|_| ())
    }

    fn transfer_progress_with_diagnostics(
        &self,
        bytes: u64,
        diagnostics: &StreamTransferDiagnostics,
    ) -> Result<()> {
        self.journal
            .db
            .update_sync_journal_stream_progress(
                self.journal.operation_id,
                &self.key,
                bytes,
                &serde_json::to_value(diagnostics)?,
            )
            .map(|_| ())
    }
}

struct ScratchGuard(PathBuf);
impl Drop for ScratchGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[allow(clippy::too_many_arguments)]
async fn stream_pack(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    plan: &RestorePlan,
    job: PackJob,
    sender: &tokio::sync::mpsc::Sender<AssemblyMessage>,
    journal: Option<DownloadJournal<'_>>,
    queue_wait_us: &Arc<AtomicU64>,
    decode_stats: &Arc<DecodeStats>,
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
    let consumer = StreamConsumer::new(
        job,
        master,
        sender.clone(),
        Arc::clone(queue_wait_us),
        Arc::clone(decode_stats),
    )?;
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
                let bytes = consumer.received_bytes();
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
    plan: impl Into<SharedRestorePlan>,
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
    let plan = plan.into();
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
    let queue_wait_us = Arc::new(AtomicU64::new(0));
    let worker_queue_wait_us = Arc::clone(&queue_wait_us);
    let decode_stats = Arc::new(DecodeStats::default());
    let worker_decode_stats = Arc::clone(&decode_stats);
    let thread = std::thread::spawn(move || {
        let result = (|| {
            // This restore-owned thread only runs the timer while awaiting input.
            // Idle downloads must still publish small ready batches and observe
            // cancellation; no filesystem work runs on the caller's async runtime.
            let receiver_runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()?;
            let mut transaction = RestoreTransaction::with_priorities(
                &worker_plan,
                &worker_roots,
                worker_db.as_ref(),
                worker_plan.priority_plan(),
                false,
            );
            let pipeline_started = Instant::now();
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
            pipeline.overlap_file_syncs = options.overlap_file_syncs;
            pipeline.queue_wait_us = worker_queue_wait_us;
            pipeline.decode_stats = worker_decode_stats;
            let needed = pipeline.consumers.keys().cloned().collect::<BTreeSet<_>>();
            init_tx
                .send(Ok(needed))
                .map_err(|_| StateError::Storage("restore cancelled".into()))?;
            loop {
                if worker_cancelled.load(Ordering::Acquire) {
                    return Err(StateError::Storage("restore cancelled".into()));
                }
                let message = match receiver_runtime.block_on(async {
                    tokio::time::timeout(PUBLICATION_DELAY, receiver.recv()).await
                }) {
                    Ok(message) => message,
                    Err(_) => {
                        pipeline.tick()?;
                        continue;
                    }
                };
                match message {
                    Some(AssemblyMessage::Chunk(hash, plain)) => pipeline.consume(&hash, &plain)?,
                    // Pack completion is not a barrier between assembly and fsync.
                    Some(AssemblyMessage::Flush) => {
                        pipeline.dispatch_assembled()?;
                        pipeline.tick()?;
                    }
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
            let mut report = pipeline.finish()?;
            transaction.publish_prerequisites(&worker_plan.prerequisites)?;
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
            report.io.commit = transaction.commit_with_metrics()?;
            report.pipeline_duration_ms = pipeline_started
                .elapsed()
                .as_millis()
                .min(u128::from(u64::MAX)) as u64;
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
    let jobs = pack_jobs(&plan, &plan.priorities, index, &needed)?;
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
            // A small compressed/deduplicated object can represent far more
            // destination work. These are selected-manifest references, not a
            // claim that already-reused files will be rewritten.
            entry.detail_json = serde_json::json!({"stream_plan": {
                "indexed_ciphertext_bytes": job.size,
                "requested_records": job.entries.len(),
                "unique_plaintext_bytes": job.entries.iter().map(|entry| u64::from(entry.plaintext_len)).sum::<u64>(),
                "selected_chunk_references": job.selected_chunk_references,
                "selected_plaintext_bytes": job.selected_plaintext_bytes,
            }});
            journal.db.upsert_sync_journal_entry(&entry)?;
        }
    }
    let transfer = run_weighted(
        jobs.into_iter()
            .map(|job| {
                let size = if provider.supports_streaming_downloads() {
                    // A pipe never retains a whole pack. Bound actual active records;
                    // the assembly channel separately bounds queued plaintext.
                    3 * noland_state_core::constants::FASTCDC_MAX
                        + 2 * decode::INPUT_BYTES as u64
                        + 1024
                } else {
                    PACK_MAX
                };
                (job, size)
            })
            .collect(),
        options.max_parallel_packs.min(32),
        options.max_inflight_pack_bytes,
        |job| {
            stream_pack(
                provider,
                master,
                &plan,
                job,
                &sender,
                journal,
                &queue_wait_us,
                &decode_stats,
            )
        },
    );
    let transfer_result = monitor_progress(transfer, &mut progress_rx, &mut on_progress).await;
    let completion_result = if transfer_result.is_ok() {
        async {
            sender
                .send(AssemblyMessage::Finalize)
                .await
                .map_err(|_| StateError::Storage("restore assembly failed".into()))?;
            monitor_progress(
                async {
                    ready_rx
                        .await
                        .map_err(|_| StateError::Storage("restore final validation failed".into()))
                },
                &mut progress_rx,
                &mut on_progress,
            )
            .await?;
            while let Ok(sample) = progress_rx.try_recv() {
                on_progress(sample)?;
            }
            sender
                .send(AssemblyMessage::Commit)
                .await
                .map_err(|_| StateError::Storage("restore assembly stopped".into()))?;
            Ok::<(), StateError>(())
        }
        .await
    } else {
        Ok(())
    };
    if transfer_result.is_err() || completion_result.is_err() {
        _guard.cancelled.store(true, Ordering::Release);
        let _ = sender.try_send(AssemblyMessage::Abort);
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
    completion_result?;
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
    use noland_state_core::{AgentPaths, AppIdentity, ChunkRef, PersistenceClass, SemanticRole};
    use noland_storage::LocalStorage;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::AtomicUsize;

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
        assert_eq!(report.io.out_of_order_files, 1);
        assert!(samples
            .windows(2)
            .all(|pair| pair[0].completed_entries <= pair[1].completed_entries));
        assert!(!plan.staging.exists());
        assert!(!plan.pack_cache.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn disk_admission_rejects_oversized_restore_before_touching_existing_saves() {
        let root = test_dir("pipeline-disk-admission");
        fs::create_dir_all(&root).unwrap();
        let mut file = test_file(
            "game/save.dat",
            PersistenceClass::PersistentState,
            SemanticRole::UserState,
            b"captured",
        );
        // Represent a legitimate large request without actually filling the disk.
        file.size = noland_state_core::disk::available(&root)
            .unwrap()
            .1
            .saturating_add(1);
        file.chunks[0].size = file.size;
        let plan = test_plan(&root, test_manifest(vec![file]));
        let home = root.join("home");
        let destination = home.join(".local/share/game/save.dat");
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::write(&destination, b"preserve original save").unwrap();
        let error = block_on(restore_pipelined(
            &LocalStorage::new(root.join("unused-remote")),
            &MasterKey::generate(),
            &plan,
            &[],
            &LogicalRootMap::from_home(&home),
            None,
            None,
            PipelineOptions::default(),
            |_| Ok(()),
        ))
        .unwrap_err();
        assert!(
            error.to_string().contains("insufficient storage"),
            "{error}"
        );
        assert_eq!(fs::read(destination).unwrap(), b"preserve original save");
        assert!(!plan.staging.exists());
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
        let first = test_plan(&root.join("first"), test_manifest(vec![file.clone()])).into_shared();
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
        let db = StateDb::open_in_memory().unwrap();
        db.upsert_app(&AppIdentity::new(
            plan.manifest.app.app_id.clone(),
            "Restore Test",
        ))
        .unwrap();
        let result = block_on(restore_pipelined(
            &LocalStorage::new(remote),
            &master,
            &plan,
            &packs[0].entries,
            &LogicalRootMap::from_home(home),
            Some(&db),
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
        assert!(db
            .list_file_states(&plan.manifest.app.app_id, None)
            .unwrap()
            .is_empty());
        assert!(db
            .associations_for_app(&plan.manifest.app.app_id)
            .unwrap()
            .is_empty());
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
        small_file_workload(4096, PipelineOptions::default().overlap_file_syncs);
    }

    #[test]
    #[ignore = "opt-in 186k-file restore publication workload"]
    fn large_small_file_publication_workload() {
        // Exercise the Linux cloud-agent path even on a non-Linux development host.
        small_file_workload(186_664, true);
    }

    fn small_file_workload(count: usize, overlap_file_syncs: bool) {
        let root = test_dir("pipeline-small-files");
        let payload = b"deduplicated small file";
        let files = (0..count)
            .map(|index| {
                test_file(
                    &format!("game/{:04}/file-{index:06}", index / 512),
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
        let started = Instant::now();
        let mut pipeline = Pipeline::new(&mut transaction, 4, |_| {
            updates += 1;
            Ok(())
        })
        .unwrap();
        pipeline.overlap_file_syncs = overlap_file_syncs;
        let checkpoint =
            fs::read(plan.staging.join(super::super::TRANSACTION_JOURNAL_FILE)).unwrap();
        let log_size = fs::metadata(plan.staging.join("restore-intents.v1.log"))
            .unwrap()
            .len();
        pipeline
            .consume(&noland_cas::blake3_hex(payload), payload)
            .unwrap();
        pipeline.flush_ready().unwrap();
        assert_eq!(pipeline.progress.completed_entries, count as u64);
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
        let report = pipeline.finish().unwrap();
        assert_eq!(report.io.file_syncs, count as u64);
        assert!(report.io.peak_unpublished_files <= MAX_UNPUBLISHED_FILES as u64);
        assert!(report.io.peak_unpublished_bytes <= MAX_UNPUBLISHED_BYTES);
        assert!(report.io.directory_syncs < count as u64 / 2);
        transaction.commit().unwrap();
        println!(
            "RESTORE_PUBLICATION {}",
            serde_json::json!({
                "files": count,
                "elapsed_ms": started.elapsed().as_millis(),
                "io": report.io,
            })
        );
        // Publication counts alone must not hide a missing or corrupt file.
        for index in 0..count {
            assert_eq!(
                fs::read(root.join(format!(
                    "home/.local/share/game/{:04}/file-{index:06}",
                    index / 512
                )))
                .unwrap(),
                payload
            );
        }
        assert!(updates >= 2);
        fs::remove_dir_all(root).unwrap();
    }
}
