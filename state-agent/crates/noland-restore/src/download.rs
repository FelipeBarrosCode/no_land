use std::collections::{BTreeMap, BTreeSet};
use std::future::{poll_fn, Future};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::Poll;

use noland_crypto::MasterKey;
use noland_pack::{extract_chunk, PackIndexEntry};
use noland_state_core::pack_key as remote_pack_key;
use noland_state_core::{ContentObjectKind, Result, StateError, SyncDirection};
use noland_state_db::StateDb;
use noland_storage::{RemoteKey, SharedStorageProvider};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{RestorePlan, RestorePriority, RestoreTarget};

pub const DEFAULT_MAX_PARALLEL_PACK_DOWNLOADS: usize = 4;

#[derive(Clone, Copy)]
pub struct DownloadJournal<'a> {
    pub db: &'a StateDb,
    pub operation_id: Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadOptions {
    pub max_parallel_packs: usize,
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            max_parallel_packs: DEFAULT_MAX_PARALLEL_PACK_DOWNLOADS,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DownloadReport {
    pub packs_downloaded: usize,
    pub packs_reused: usize,
    pub chunks_extracted: usize,
    pub chunks_reused: usize,
}

impl DownloadReport {
    fn merge(&mut self, other: Self) {
        self.packs_downloaded += other.packs_downloaded;
        self.packs_reused += other.packs_reused;
        self.chunks_extracted += other.chunks_extracted;
        self.chunks_reused += other.chunks_reused;
    }
}

#[derive(Debug)]
struct PackJob {
    pack_id: String,
    priority: RestorePriority,
    entries: Vec<PackIndexEntry>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct VerifiedPack {
    pack_id: String,
    chunks: BTreeSet<String>,
}

pub async fn download_and_verify_to(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    plan: &RestorePlan,
    pack_index: &[PackIndexEntry],
    target: RestoreTarget,
    options: DownloadOptions,
    journal: Option<DownloadJournal<'_>>,
) -> Result<DownloadReport> {
    if options.max_parallel_packs == 0 {
        return Err(StateError::Invalid(
            "max_parallel_packs must be greater than zero".into(),
        ));
    }

    let priority_plan = plan.priority_plan();
    let mut needed = BTreeMap::<String, RestorePriority>::new();
    for planned in priority_plan.entries_for(target) {
        for chunk in &plan.manifest.files[planned.manifest_index].chunks {
            needed
                .entry(chunk.hash.clone())
                .and_modify(|priority| *priority = (*priority).min(planned.priority))
                .or_insert(planned.priority);
        }
    }

    let mut indexed = pack_index.to_vec();
    indexed.sort_by(|left, right| {
        left.chunk_hash
            .cmp(&right.chunk_hash)
            .then_with(|| left.pack_id.cmp(&right.pack_id))
            .then_with(|| left.offset.cmp(&right.offset))
    });
    let by_hash = indexed
        .into_iter()
        .map(|entry| (entry.chunk_hash.clone(), entry))
        .collect::<BTreeMap<_, _>>();

    let mut grouped = BTreeMap::<String, PackJob>::new();
    for (chunk_hash, priority) in needed {
        let entry = by_hash.get(&chunk_hash).cloned().ok_or_else(|| {
            StateError::NotFound(format!("pack index entry for chunk {chunk_hash}"))
        })?;
        let job = grouped
            .entry(entry.pack_id.clone())
            .or_insert_with(|| PackJob {
                pack_id: entry.pack_id.clone(),
                priority,
                entries: Vec::new(),
            });
        job.priority = job.priority.min(priority);
        job.entries.push(entry);
    }

    let mut jobs = grouped.into_values().collect::<Vec<_>>();
    for job in &mut jobs {
        job.entries.sort_by(|left, right| {
            left.offset
                .cmp(&right.offset)
                .then_with(|| left.chunk_hash.cmp(&right.chunk_hash))
        });
    }
    jobs.sort_by(|left, right| {
        left.priority
            .cmp(&right.priority)
            .then_with(|| left.pack_id.cmp(&right.pack_id))
    });

    run_bounded(jobs, options.max_parallel_packs, |job| {
        process_pack(provider, master, plan, job, journal)
    })
    .await
}

async fn process_pack(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    plan: &RestorePlan,
    job: PackJob,
    journal: Option<DownloadJournal<'_>>,
) -> Result<DownloadReport> {
    let mut report = DownloadReport::default();
    let mut missing = Vec::new();
    for entry in &job.entries {
        let chunk_path = chunk_path(plan, &entry.chunk_hash);
        if verified_file(&chunk_path, entry.plaintext_len as u64, &entry.chunk_hash) {
            report.chunks_reused += 1;
        } else {
            remove_file_if_present(&chunk_path)?;
            missing.push(entry.clone());
        }
    }
    if missing.is_empty() {
        return Ok(report);
    }

    let cache_path = pack_cache_path(plan, &job.pack_id);
    let stage_path = plan
        .staging
        .join("packs")
        .join(format!("{}.pack", job.pack_id));
    let remote_key = remote_pack_key(&job.pack_id);
    let journal_completed = match journal {
        Some(journal) => journal
            .db
            .sync_journal_completed(journal.operation_id, &remote_key)?,
        None => false,
    };
    let reused_cache = cache_path.is_file();
    if reused_cache {
        report.packs_reused += 1;
        if let Some(journal) = journal {
            if !journal_completed {
                mark_pack_completed(journal, &remote_key, &cache_path)?;
            }
        }
    } else {
        download_pack_journaled(
            provider,
            plan,
            &job.pack_id,
            &cache_path,
            &remote_key,
            journal,
        )
        .await?;
        report.packs_downloaded += 1;
    }
    link_staged_pack(&cache_path, &stage_path)?;

    match extract_missing(plan, master, &cache_path, &missing) {
        Ok(extracted) => report.chunks_extracted += extracted,
        Err(error) if reused_cache => {
            remove_file_if_present(&cache_path)?;
            remove_file_if_present(&verified_marker_path(&cache_path))?;
            remove_file_if_present(&stage_path)?;
            download_pack_journaled(
                provider,
                plan,
                &job.pack_id,
                &cache_path,
                &remote_key,
                journal,
            )
            .await?;
            report.packs_reused = report.packs_reused.saturating_sub(1);
            report.packs_downloaded += 1;
            link_staged_pack(&cache_path, &stage_path)?;
            report.chunks_extracted += extract_missing(plan, master, &cache_path, &missing)
                .map_err(|retry| StateError::Integrity(format!(
                    "cached pack {} was invalid ({error}); downloaded replacement also failed ({retry})",
                    job.pack_id
                )))?;
        }
        Err(error) => return Err(error),
    }

    record_verified_chunks(&cache_path, &job.pack_id, &job.entries)?;
    Ok(report)
}

async fn download_pack_journaled(
    provider: &dyn SharedStorageProvider,
    plan: &RestorePlan,
    pack_id: &str,
    cache_path: &Path,
    remote_key: &str,
    journal: Option<DownloadJournal<'_>>,
) -> Result<()> {
    if let Some(journal) = journal {
        journal.db.start_sync_journal_item(
            journal.operation_id,
            remote_key,
            ContentObjectKind::Pack,
            SyncDirection::Download,
            Some(&cache_path.to_string_lossy()),
            Some(remote_key),
            None,
        )?;
    }
    match download_pack(provider, plan, pack_id, cache_path).await {
        Ok(()) => {
            if let Some(journal) = journal {
                let size = std::fs::metadata(cache_path)
                    .map(|meta| meta.len())
                    .unwrap_or(0);
                journal
                    .db
                    .complete_sync_journal_item(journal.operation_id, remote_key, size)?;
            }
            Ok(())
        }
        Err(error) => {
            if let Some(journal) = journal {
                let _ = journal.db.fail_sync_journal_item(
                    journal.operation_id,
                    remote_key,
                    &error.to_string(),
                );
            }
            Err(error)
        }
    }
}

fn mark_pack_completed(
    journal: DownloadJournal<'_>,
    remote_key: &str,
    cache_path: &Path,
) -> Result<()> {
    journal.db.start_sync_journal_item(
        journal.operation_id,
        remote_key,
        ContentObjectKind::Pack,
        SyncDirection::Download,
        Some(&cache_path.to_string_lossy()),
        Some(remote_key),
        std::fs::metadata(cache_path).ok().map(|meta| meta.len()),
    )?;
    let size = std::fs::metadata(cache_path)
        .map(|meta| meta.len())
        .unwrap_or(0);
    journal
        .db
        .complete_sync_journal_item(journal.operation_id, remote_key, size)
}

async fn download_pack(
    provider: &dyn SharedStorageProvider,
    plan: &RestorePlan,
    pack_id: &str,
    cache_path: &Path,
) -> Result<()> {
    if let Some(parent) = cache_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = cache_path.with_extension(format!("pack.{}.partial", plan.restore_id));
    remove_file_if_present(&temp)?;
    let result = provider
        .download(&RemoteKey::new(remote_pack_key(pack_id)), &temp)
        .await;
    if let Err(error) = result {
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }
    if cache_path.exists() {
        remove_file_if_present(&temp)?;
    } else {
        std::fs::rename(&temp, cache_path)?;
    }
    Ok(())
}

fn extract_missing(
    plan: &RestorePlan,
    master: &MasterKey,
    pack_path: &Path,
    entries: &[PackIndexEntry],
) -> Result<usize> {
    let mut extracted = 0;
    for entry in entries {
        let chunk_path = chunk_path(plan, &entry.chunk_hash);
        if verified_file(&chunk_path, entry.plaintext_len as u64, &entry.chunk_hash) {
            continue;
        }
        let plain = extract_chunk(pack_path, entry, master)?;
        if let Some(parent) = chunk_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temp = chunk_path.with_extension(format!("{}.partial", plan.restore_id));
        remove_file_if_present(&temp)?;
        std::fs::write(&temp, plain)?;
        if chunk_path.exists() {
            remove_file_if_present(&chunk_path)?;
        }
        std::fs::rename(temp, chunk_path)?;
        extracted += 1;
    }
    Ok(extracted)
}

fn record_verified_chunks(
    cache_path: &Path,
    pack_id: &str,
    entries: &[PackIndexEntry],
) -> Result<()> {
    let marker = verified_marker_path(cache_path);
    let mut verified = std::fs::read(&marker)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<VerifiedPack>(&bytes).ok())
        .filter(|value| value.pack_id == pack_id)
        .unwrap_or_else(|| VerifiedPack {
            pack_id: pack_id.to_string(),
            chunks: BTreeSet::new(),
        });
    verified
        .chunks
        .extend(entries.iter().map(|entry| entry.chunk_hash.clone()));
    let temp = marker.with_extension("verified.partial");
    std::fs::write(&temp, serde_json::to_vec(&verified)?)?;
    if marker.exists() {
        remove_file_if_present(&marker)?;
    }
    std::fs::rename(temp, marker)?;
    Ok(())
}

fn link_staged_pack(cache_path: &Path, stage_path: &Path) -> Result<()> {
    if stage_path.exists() {
        return Ok(());
    }
    if let Some(parent) = stage_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::hard_link(cache_path, stage_path).is_err() {
        std::fs::copy(cache_path, stage_path)?;
    }
    Ok(())
}

fn pack_cache_path(plan: &RestorePlan, pack_id: &str) -> PathBuf {
    let prefix = pack_id.chars().take(2).collect::<String>();
    plan.pack_cache.join(prefix).join(format!("{pack_id}.pack"))
}

fn verified_marker_path(pack_path: &Path) -> PathBuf {
    pack_path.with_extension("pack.verified.json")
}

fn chunk_path(plan: &RestorePlan, hash: &str) -> PathBuf {
    plan.staging
        .join("materialized/.chunks")
        .join(hash.trim_start_matches("blake3:"))
}

fn verified_file(path: &Path, expected_size: u64, expected_hash: &str) -> bool {
    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.len() == expected_size)
        .unwrap_or(false)
        && noland_cas::blake3_file(path)
            .map(|hash| hash == expected_hash)
            .unwrap_or(false)
}

fn remove_file_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

async fn run_bounded<T, F, Fut>(
    items: Vec<T>,
    limit: usize,
    mut operation: F,
) -> Result<DownloadReport>
where
    F: FnMut(T) -> Fut,
    Fut: Future<Output = Result<DownloadReport>>,
{
    let mut items = items.into_iter();
    let mut report = DownloadReport::default();
    loop {
        let futures = items
            .by_ref()
            .take(limit)
            .map(&mut operation)
            .collect::<Vec<_>>();
        if futures.is_empty() {
            break;
        }
        for completed in try_join_all(futures).await? {
            report.merge(completed);
        }
    }
    Ok(report)
}

async fn try_join_all<F, T>(futures: Vec<F>) -> Result<Vec<T>>
where
    F: Future<Output = Result<T>>,
{
    let mut futures = futures
        .into_iter()
        .map(|future| Some(Box::pin(future)))
        .collect::<Vec<Option<Pin<Box<F>>>>>();
    let mut completed = (0..futures.len()).map(|_| None).collect::<Vec<_>>();

    poll_fn(|context| {
        let mut pending = false;
        for (index, slot) in futures.iter_mut().enumerate() {
            let Some(mut future) = slot.take() else {
                continue;
            };
            match future.as_mut().poll(context) {
                Poll::Ready(Ok(value)) => completed[index] = Some(value),
                Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                Poll::Pending => {
                    *slot = Some(future);
                    pending = true;
                }
            }
        }
        if pending {
            Poll::Pending
        } else {
            Poll::Ready(Ok(completed
                .iter_mut()
                .map(|value| value.take().expect("completed future has a value"))
                .collect()))
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::task::{Context, Wake, Waker};

    use super::*;

    #[test]
    fn bounded_runner_never_exceeds_limit() {
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let report = block_on(run_bounded((0..9).collect(), 3, |_| YieldOnce {
            active: Arc::clone(&active),
            maximum: Arc::clone(&maximum),
            started: false,
        }))
        .unwrap();

        assert_eq!(maximum.load(Ordering::SeqCst), 3);
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert_eq!(report, DownloadReport::default());
    }

    struct YieldOnce {
        active: Arc<AtomicUsize>,
        maximum: Arc<AtomicUsize>,
        started: bool,
    }

    impl Future for YieldOnce {
        type Output = Result<DownloadReport>;

        fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
            if !self.started {
                self.started = true;
                let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
                self.maximum.fetch_max(active, Ordering::SeqCst);
                context.waker().wake_by_ref();
                Poll::Pending
            } else {
                self.active.fetch_sub(1, Ordering::SeqCst);
                Poll::Ready(Ok(DownloadReport::default()))
            }
        }
    }

    struct NoopWaker;

    impl Wake for NoopWaker {
        fn wake(self: Arc<Self>) {}
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        let waker = Waker::from(Arc::new(NoopWaker));
        let mut context = Context::from_waker(&waker);
        let mut future = Box::pin(future);
        loop {
            match future.as_mut().poll(&mut context) {
                Poll::Ready(output) => return output,
                Poll::Pending => std::thread::yield_now(),
            }
        }
    }
}
