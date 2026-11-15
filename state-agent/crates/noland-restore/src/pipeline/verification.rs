//! Restore-owned workers. Only completed partials cross this boundary; workers
//! never publish paths or touch the database. Drop joins them before rollback.
use std::fs;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc, Arc, Condvar, Mutex,
};
use std::time::{Duration, Instant};

use noland_state_core::{ManifestFile, Result, StateError};

use super::{
    elapsed_us, open_partial, PendingFile, PipelineProgress, ReadbackCause, MAX_UNPUBLISHED_FILES,
};
use crate::identity::VerifiedIdentity;

pub(super) const BUFFER_BYTES: usize = 256 * 1024;

pub(super) struct VerificationTask {
    pub pending: PendingFile,
    size: u64,
    hash: String,
    mode: Option<u32>,
}

impl VerificationTask {
    pub fn new(pending: PendingFile, file: &ManifestFile) -> Self {
        Self {
            pending,
            size: file.size,
            hash: file.file_hash.clone(),
            mode: file.mode,
        }
    }
}

pub(super) struct VerifiedFile {
    pub pending: PendingFile,
    pub identity: VerifiedIdentity,
}

#[derive(Default)]
pub(super) struct VerificationStats {
    readback_bytes: AtomicU64,
    readback_out_of_order_bytes: AtomicU64,
    readback_hash_eviction_bytes: AtomicU64,
    readback_missing_hash_bytes: AtomicU64,
    verified_bytes: AtomicU64,
    verified_files: AtomicU64,
    verify_us: AtomicU64,
    sync_us: AtomicU64,
    sync_publication_wait_us: AtomicU64,
    sync_gate: Arc<FileSyncGate>,
    active: AtomicU64,
    peak: AtomicU64,
}

struct ActiveVerification(Arc<VerificationStats>);

impl Drop for ActiveVerification {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::Relaxed);
    }
}

#[derive(Default)]
struct FileSyncState {
    publishing: bool,
    #[cfg(test)]
    waiting: usize,
}

/// Publication never waits for an already running file sync. Only NEW syncs
/// pause, so a ready file need not wait for the slowest member of its cohort.
#[derive(Default)]
struct FileSyncGate {
    state: Mutex<FileSyncState>,
    changed: Condvar,
}

pub(super) struct PublicationPause(Arc<FileSyncGate>);

impl Drop for PublicationPause {
    fn drop(&mut self) {
        self.0.resume();
    }
}

impl FileSyncGate {
    fn pause(self: &Arc<Self>) -> Result<PublicationPause> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| StateError::Storage("restore sync gate poisoned".into()))?;
        if state.publishing {
            return Err(StateError::Storage(
                "restore publication already in progress".into(),
            ));
        }
        state.publishing = true;
        Ok(PublicationPause(Arc::clone(self)))
    }

    fn resume(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.publishing = false;
        }
        self.changed.notify_all();
    }

    fn before_sync(&self, stopped: &AtomicBool, wait_us: &AtomicU64) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| StateError::Storage("restore sync gate poisoned".into()))?;
        if state.publishing {
            let started = Instant::now();
            #[cfg(test)]
            {
                state.waiting += 1;
                self.changed.notify_all();
            }
            while state.publishing && !stopped.load(Ordering::Acquire) {
                state = self
                    .changed
                    .wait(state)
                    .map_err(|_| StateError::Storage("restore sync gate poisoned".into()))?;
            }
            #[cfg(test)]
            {
                state.waiting -= 1;
            }
            wait_us.fetch_add(elapsed_us(started), Ordering::Relaxed);
        }
        if stopped.load(Ordering::Acquire) {
            return Err(StateError::Storage("restore verification cancelled".into()));
        }
        Ok(())
    }
}

pub(super) fn verify_file(
    task: &VerificationTask,
    stats: &VerificationStats,
    stopped: &AtomicBool,
    buffer: &mut [u8],
) -> Result<VerifiedIdentity> {
    let started = Instant::now();
    let pending = &task.pending;
    super::validate_parent_chain(&pending.planned.root, &pending.partial)?;
    let (mut output, metadata) = open_partial(pending)?;
    let assembled = VerifiedIdentity::from_metadata(&metadata);
    if pending
        .last_write
        .is_some_and(|last_write| last_write != assembled)
    {
        return Err(StateError::Integrity(
            "restore partial changed before verification".into(),
        ));
    }
    if pending.received != task.size || metadata.len() != task.size {
        return Err(StateError::Integrity(
            "reconstructed file size mismatch".into(),
        ));
    }
    let changed_mode = task
        .mode
        .filter(|mode| metadata.mode() & 0o7777 != mode & 0o7777);
    if let Some(mode) = changed_mode {
        output.set_permissions(fs::Permissions::from_mode(mode))?;
    }
    let before = if changed_mode.is_some() {
        VerifiedIdentity::from_metadata(&output.metadata()?)
    } else {
        assembled
    };
    // Permission application is the only intended change before hashing.
    if (changed_mode.is_none() && assembled != before)
        || !assembled
            .with_mode(before.mode)
            .matches_after_rename(before)
    {
        return Err(StateError::Integrity(
            "restore partial changed while setting permissions".into(),
        ));
    }
    let digest = if pending.readback || (pending.received != 0 && pending.hasher.is_none()) {
        let mut hasher = blake3::Hasher::new();
        let cause_bytes = match pending.readback_cause {
            Some(ReadbackCause::OutOfOrder) => &stats.readback_out_of_order_bytes,
            Some(ReadbackCause::HashEviction) => &stats.readback_hash_eviction_bytes,
            None => &stats.readback_missing_hash_bytes,
        };
        loop {
            if stopped.load(Ordering::Acquire) {
                return Err(StateError::Storage("restore verification cancelled".into()));
            }
            let count = output.read(buffer)?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
            stats
                .readback_bytes
                .fetch_add(count as u64, Ordering::Relaxed);
            cause_bytes.fetch_add(count as u64, Ordering::Relaxed);
        }
        hasher.finalize()
    } else {
        pending.hasher.as_ref().map_or_else(
            || blake3::Hasher::new().finalize(),
            |hasher| hasher.finalize(),
        )
    };
    if format!("blake3:{}", digest.to_hex()) != task.hash {
        return Err(StateError::Integrity(
            "reconstructed file hash mismatch".into(),
        ));
    }
    stats
        .verify_us
        .fetch_add(elapsed_us(started), Ordering::Relaxed);
    if stopped.load(Ordering::Acquire) {
        return Err(StateError::Storage("restore verification cancelled".into()));
    }
    stats
        .sync_gate
        .before_sync(stopped, &stats.sync_publication_wait_us)?;
    let sync_started = Instant::now();
    // A directory flush does not make file contents durable. Keep the file
    // barrier, but overlap it with assembly of the bounded next window.
    output.sync_all()?;
    let after = VerifiedIdentity::from_metadata(&output.metadata()?);
    if before != after || !after.matches_path(&pending.partial) {
        return Err(StateError::Integrity(
            "restore partial changed during verification".into(),
        ));
    }
    stats
        .sync_us
        .fetch_add(elapsed_us(sync_started), Ordering::Relaxed);
    stats.verified_bytes.fetch_add(task.size, Ordering::Relaxed);
    stats.verified_files.fetch_add(1, Ordering::Relaxed);
    Ok(after)
}

pub(super) struct VerificationPool {
    jobs: Option<mpsc::SyncSender<VerificationTask>>,
    completed: mpsc::Receiver<Result<VerifiedFile>>,
    handles: Vec<std::thread::JoinHandle<()>>,
    stopped: Arc<AtomicBool>,
    stats: Arc<VerificationStats>,
}

impl VerificationPool {
    pub fn new(workers: usize) -> Result<Self> {
        Self::start(workers, verify_file)
    }

    pub fn start(
        workers: usize,
        verify: impl Fn(
                &VerificationTask,
                &VerificationStats,
                &AtomicBool,
                &mut [u8],
            ) -> Result<VerifiedIdentity>
            + Send
            + Sync
            + 'static,
    ) -> Result<Self> {
        let (jobs, receiver) = mpsc::sync_channel::<VerificationTask>(MAX_UNPUBLISHED_FILES);
        let receiver = Arc::new(Mutex::new(receiver));
        // The assembler bounds ALL unpublished files, including queued jobs,
        // running jobs and these results. Results must not block worker shutdown.
        let (completed, results) = mpsc::channel();
        let mut pool = Self {
            jobs: Some(jobs),
            completed: results,
            handles: Vec::new(),
            stopped: Arc::new(AtomicBool::new(false)),
            stats: Arc::new(VerificationStats::default()),
        };
        let verify = Arc::new(verify);
        // Production admission already apportions the shared resource budget.
        // Do not hide another CPU ceiling here; explicit fixture pools must also
        // remain deterministic independently of the test runner's host size.
        for index in 0..workers.max(1) {
            let receiver = Arc::clone(&receiver);
            let completed = completed.clone();
            let stopped = Arc::clone(&pool.stopped);
            let stats = Arc::clone(&pool.stats);
            let verify = Arc::clone(&verify);
            pool.handles.push(
                std::thread::Builder::new()
                    .name(format!("restore-verify-{index}"))
                    .spawn(move || {
                        let mut buffer = vec![0u8; BUFFER_BYTES];
                        loop {
                            let task = match receiver.lock() {
                                Ok(receiver) => receiver.recv(),
                                Err(_) => return,
                            };
                            let Ok(task) = task else { return };
                            if stopped.load(Ordering::Acquire) {
                                return;
                            }
                            let active = stats.active.fetch_add(1, Ordering::Relaxed) + 1;
                            stats.peak.fetch_max(active, Ordering::Relaxed);
                            let active = ActiveVerification(Arc::clone(&stats));
                            // Turn a worker panic into a result: otherwise one
                            // lost task could leave the assembler waiting forever.
                            let result = catch_unwind(AssertUnwindSafe(|| {
                                verify(&task, &stats, &stopped, &mut buffer)
                            }))
                            .unwrap_or_else(|_| {
                                Err(StateError::Storage(
                                    "restore verification worker panicked".into(),
                                ))
                            })
                            .map(|identity| VerifiedFile {
                                pending: task.pending,
                                identity,
                            });
                            drop(active);
                            let failed = result.is_err();
                            if completed.send(result).is_err() || failed {
                                return;
                            }
                        }
                    })?,
            );
        }
        Ok(pool)
    }

    pub fn submit(&self, task: VerificationTask) -> Result<()> {
        // Admission happens before submit, so this bounded queue cannot be full.
        self.jobs
            .as_ref()
            .expect("live verification pool")
            .try_send(task)
            .map_err(|_| StateError::Storage("restore verification queue unavailable".into()))
    }

    pub fn workers(&self) -> usize {
        self.handles.len()
    }

    pub fn pause_file_syncs(&self) -> Result<PublicationPause> {
        self.stats.sync_gate.pause()
    }

    #[cfg(test)]
    pub fn wait_for_paused_syncs(&self, count: usize, timeout: Duration) -> bool {
        let gate = &self.stats.sync_gate;
        let (state, _) = gate
            .changed
            .wait_timeout_while(gate.state.lock().unwrap(), timeout, |state| {
                state.waiting < count
            })
            .unwrap();
        state.waiting >= count
    }

    pub fn receive(&self, timeout: Duration) -> Result<Option<VerifiedFile>> {
        match self.completed.recv_timeout(timeout) {
            Ok(result) => result.map(Some),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(StateError::Storage(
                "restore verification workers stopped".into(),
            )),
        }
    }

    pub fn snapshot(&self, progress: &mut PipelineProgress) {
        progress.readback_bytes = self.stats.readback_bytes.load(Ordering::Relaxed);
        progress.io.readback_bytes = progress.readback_bytes;
        progress.io.readback_out_of_order_bytes = self
            .stats
            .readback_out_of_order_bytes
            .load(Ordering::Relaxed);
        progress.io.readback_hash_eviction_bytes = self
            .stats
            .readback_hash_eviction_bytes
            .load(Ordering::Relaxed);
        progress.io.readback_missing_hash_bytes = self
            .stats
            .readback_missing_hash_bytes
            .load(Ordering::Relaxed);
        progress.verified_bytes = self.stats.verified_bytes.load(Ordering::Relaxed);
        progress.io.file_syncs = self.stats.verified_files.load(Ordering::Relaxed);
        progress.io.verify_us = self.stats.verify_us.load(Ordering::Relaxed);
        progress.io.file_sync_us = self.stats.sync_us.load(Ordering::Relaxed);
        progress.io.file_sync_publication_wait_us =
            self.stats.sync_publication_wait_us.load(Ordering::Relaxed);
        progress.io.peak_verification_workers = self.stats.peak.load(Ordering::Relaxed);
    }
}

impl Drop for VerificationPool {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        self.stats.sync_gate.resume();
        self.jobs.take();
        // Join EVERY worker, including after a failure or callback cancellation,
        // before the transaction can roll back or remove any partial paths.
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
    }
}
