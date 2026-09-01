use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use noland_rclone_adapter::{
    classify_remote_error, ProviderRootIdentity, RemoteErrorClass, TransferTuning,
};
use noland_state_core::{Result, StateError};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio::task::JoinSet;
use tokio::time::Instant;

use crate::adaptive::AdaptiveConcurrency;
use crate::{RemoteKey, RemoteMeta, SharedStorageProvider};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImmutableUpload {
    pub local: PathBuf,
    pub key: RemoteKey,
}

impl ImmutableUpload {
    pub fn new(local: impl Into<PathBuf>, key: RemoteKey) -> Self {
        Self {
            local: local.into(),
            key,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadRequest {
    pub key: RemoteKey,
    pub destination: PathBuf,
}

impl DownloadRequest {
    pub fn new(key: RemoteKey, destination: impl Into<PathBuf>) -> Self {
        Self {
            key,
            destination: destination.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MetadataWrite {
    pub key: RemoteKey,
    pub bytes: Bytes,
}

impl MetadataWrite {
    pub fn new(key: RemoteKey, bytes: impl Into<Bytes>) -> Self {
        Self {
            key,
            bytes: bytes.into(),
        }
    }
}

/// Metadata writes split into an ordinary phase and an optional visibility
/// marker. Providers must finish every `entries` write before `committed`.
#[derive(Debug, Clone, Default)]
pub struct MetadataBatch {
    entries: Vec<MetadataWrite>,
    committed: Option<MetadataWrite>,
}

impl MetadataBatch {
    pub fn new(entries: Vec<MetadataWrite>) -> Self {
        Self {
            entries,
            committed: None,
        }
    }

    pub fn with_committed(entries: Vec<MetadataWrite>, committed: MetadataWrite) -> Self {
        Self {
            entries,
            committed: Some(committed),
        }
    }

    pub fn entries(&self) -> &[MetadataWrite] {
        &self.entries
    }

    pub fn committed(&self) -> Option<&MetadataWrite> {
        self.committed.as_ref()
    }

    pub fn total_len(&self) -> usize {
        self.entries.len() + usize::from(self.committed.is_some())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteSizeConflict {
    pub key: RemoteKey,
    pub local_size: u64,
    pub remote_size: u64,
}

#[derive(Debug, Clone, Default)]
pub struct RemoteKnownComparison {
    pub known: Vec<RemoteMeta>,
    pub missing: Vec<ImmutableUpload>,
    pub size_conflicts: Vec<RemoteSizeConflict>,
}

#[derive(Debug, Clone)]
pub struct RemoteKnownSet {
    pub prefix: RemoteKey,
    entries: HashMap<RemoteKey, u64>,
}

impl RemoteKnownSet {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn size(&self, key: &RemoteKey) -> Option<u64> {
        self.entries.get(key).copied()
    }

    pub fn contains(&self, key: &RemoteKey) -> bool {
        self.entries.contains_key(key)
    }

    pub fn compare(&self, candidates: &[ImmutableUpload]) -> Result<RemoteKnownComparison> {
        compare_known_map(&self.entries, candidates)
    }
}

/// Lists all file keys under a prefix once for reusable local comparisons.
pub async fn list_remote_known(
    provider: &dyn SharedStorageProvider,
    prefix: &RemoteKey,
) -> Result<RemoteKnownSet> {
    let entries = provider
        .list_prefix(prefix)
        .await?
        .into_iter()
        .filter(|entry| !entry.is_prefix)
        .map(|entry| (entry.key, entry.size))
        .collect();
    Ok(RemoteKnownSet {
        prefix: prefix.clone(),
        entries,
    })
}

/// Lists a prefix once and compares exact keys and sizes locally.
pub async fn compare_remote_known(
    provider: &dyn SharedStorageProvider,
    prefix: &RemoteKey,
    candidates: &[ImmutableUpload],
) -> Result<RemoteKnownComparison> {
    list_remote_known(provider, prefix)
        .await?
        .compare(candidates)
}

pub(crate) fn compare_known_map(
    known_by_key: &HashMap<RemoteKey, u64>,
    candidates: &[ImmutableUpload],
) -> Result<RemoteKnownComparison> {
    let mut comparison = RemoteKnownComparison::default();
    let mut seen = HashSet::new();
    for candidate in candidates {
        validate_remote_key(&candidate.key)?;
        if !seen.insert(candidate.key.clone()) {
            return Err(StateError::Invalid(format!(
                "duplicate immutable upload key: {}",
                candidate.key.as_str()
            )));
        }
        let local_size = std::fs::metadata(&candidate.local)?.len();
        match known_by_key.get(&candidate.key).copied() {
            Some(remote_size) if remote_size == local_size => comparison.known.push(RemoteMeta {
                key: candidate.key.clone(),
                size: remote_size,
            }),
            Some(remote_size) => comparison.size_conflicts.push(RemoteSizeConflict {
                key: candidate.key.clone(),
                local_size,
                remote_size,
            }),
            None => comparison.missing.push(candidate.clone()),
        }
    }
    Ok(comparison)
}

pub(crate) fn validate_remote_key(key: &RemoteKey) -> Result<()> {
    let raw = key.as_str();
    let path = std::path::Path::new(raw);
    if raw.trim().is_empty()
        || raw.starts_with('/')
        || raw.starts_with('\\')
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(StateError::UnsafePath(raw.to_string()));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransferDirection {
    Upload,
    Download,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransferJournalState {
    Started,
    Succeeded,
    Skipped,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransferJournalEvent {
    pub direction: TransferDirection,
    pub key: RemoteKey,
    pub state: TransferJournalState,
    pub attempt: u32,
    pub error: Option<String>,
}

pub type ShouldTransferCallback =
    dyn Fn(TransferDirection, &RemoteKey) -> Result<bool> + Send + Sync + 'static;
pub type RecordTransferCallback =
    dyn Fn(&TransferJournalEvent) -> Result<()> + Send + Sync + 'static;

/// Caller-owned resumability hooks. A DB-backed caller can skip completed keys
/// and persist every state transition without coupling storage to a DB schema.
#[derive(Clone, Default)]
pub struct TransferJournalCallbacks {
    should_transfer: Option<Arc<ShouldTransferCallback>>,
    record: Option<Arc<RecordTransferCallback>>,
}

impl TransferJournalCallbacks {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_should_transfer<F>(mut self, callback: F) -> Self
    where
        F: Fn(TransferDirection, &RemoteKey) -> Result<bool> + Send + Sync + 'static,
    {
        self.should_transfer = Some(Arc::new(callback));
        self
    }

    pub fn with_record<F>(mut self, callback: F) -> Self
    where
        F: Fn(&TransferJournalEvent) -> Result<()> + Send + Sync + 'static,
    {
        self.record = Some(Arc::new(callback));
        self
    }

    pub fn should_transfer(&self, direction: TransferDirection, key: &RemoteKey) -> Result<bool> {
        self.should_transfer
            .as_ref()
            .map(|callback| callback(direction, key))
            .unwrap_or(Ok(true))
    }

    pub fn record(&self, event: &TransferJournalEvent) -> Result<()> {
        self.record
            .as_ref()
            .map(|callback| callback(event))
            .unwrap_or(Ok(()))
    }
}

#[derive(Debug, Clone, Default)]
pub struct UploadBatchReport {
    pub uploaded: Vec<RemoteMeta>,
    pub skipped: Vec<RemoteKey>,
}

#[derive(Debug, Clone, Default)]
pub struct DownloadBatchReport {
    pub downloaded: Vec<RemoteKey>,
    pub skipped: Vec<RemoteKey>,
}

/// Shared request pacing and exponential backoff gate. Clones share one remote
/// cooldown, preventing parallel workers from independently hammering a limit.
pub struct SharedRetryGate {
    tuning: TransferTuning,
    next_request: Mutex<Instant>,
}

impl SharedRetryGate {
    pub fn new(tuning: TransferTuning) -> Self {
        Self {
            tuning: tuning.normalized(),
            next_request: Mutex::new(Instant::now()),
        }
    }

    pub fn tuning(&self) -> &TransferTuning {
        &self.tuning
    }

    pub async fn wait_for_turn(&self) {
        let deadline = {
            let mut next = self.next_request.lock().await;
            let now = Instant::now();
            let deadline = (*next).max(now);
            *next = deadline + Duration::from_millis(self.tuning.min_request_interval_ms);
            deadline
        };
        tokio::time::sleep_until(deadline).await;
    }

    pub async fn wait_for_retry(&self, failed_attempt: u32, class: RemoteErrorClass) {
        let delay = Duration::from_millis(
            self.tuning
                .backoff_ms(failed_attempt, class.is_rate_limited()),
        );
        let deadline = Instant::now() + delay;
        {
            let mut next = self.next_request.lock().await;
            if deadline > *next {
                *next = deadline;
            }
        }
        tokio::time::sleep_until(deadline).await;
    }
}

pub async fn upload_immutable_bounded<P>(
    provider: Arc<P>,
    uploads: Vec<ImmutableUpload>,
    tuning: TransferTuning,
    journal: TransferJournalCallbacks,
) -> Result<UploadBatchReport>
where
    P: SharedStorageProvider + ?Sized + 'static,
{
    let tuning = tuning.normalized();
    let gate = Arc::new(SharedRetryGate::new(tuning.clone()));
    let limiter = AdaptiveConcurrency::for_uploads(&tuning);
    let mut tasks = JoinSet::new();
    let mut skipped = Vec::new();

    for (index, upload) in uploads.into_iter().enumerate() {
        validate_remote_key(&upload.key)?;
        if !journal.should_transfer(TransferDirection::Upload, &upload.key)? {
            journal.record(&TransferJournalEvent {
                direction: TransferDirection::Upload,
                key: upload.key.clone(),
                state: TransferJournalState::Skipped,
                attempt: 0,
                error: None,
            })?;
            skipped.push(upload.key);
            continue;
        }
        let provider = Arc::clone(&provider);
        let gate = Arc::clone(&gate);
        let limiter = Arc::clone(&limiter);
        let journal = journal.clone();
        let max_attempts = tuning.max_attempts;
        tasks.spawn(async move {
            let _permit = limiter.acquire().await?;
            let key = upload.key.clone();
            let result = retry_transfer(
                &gate,
                Some(&limiter),
                max_attempts,
                TransferDirection::Upload,
                &key,
                &journal,
                || provider.upload_immutable(&upload.local, &upload.key),
            )
            .await;
            result.map(|meta| (index, meta))
        });
    }

    let mut uploaded = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok(Ok(item)) => uploaded.push(item),
            Ok(Err(error)) => {
                tasks.abort_all();
                return Err(error);
            }
            Err(error) => {
                tasks.abort_all();
                return Err(StateError::Storage(format!(
                    "upload worker failed: {error}"
                )));
            }
        }
    }
    uploaded.sort_by_key(|(index, _)| *index);
    Ok(UploadBatchReport {
        uploaded: uploaded.into_iter().map(|(_, meta)| meta).collect(),
        skipped,
    })
}

pub async fn download_bounded<P>(
    provider: Arc<P>,
    downloads: Vec<DownloadRequest>,
    tuning: TransferTuning,
    journal: TransferJournalCallbacks,
) -> Result<DownloadBatchReport>
where
    P: SharedStorageProvider + ?Sized + 'static,
{
    let tuning = tuning.normalized();
    let gate = Arc::new(SharedRetryGate::new(tuning.clone()));
    let limiter = AdaptiveConcurrency::for_downloads(&tuning);
    let mut tasks = JoinSet::new();
    let mut skipped = Vec::new();

    for (index, download) in downloads.into_iter().enumerate() {
        validate_remote_key(&download.key)?;
        if !journal.should_transfer(TransferDirection::Download, &download.key)? {
            journal.record(&TransferJournalEvent {
                direction: TransferDirection::Download,
                key: download.key.clone(),
                state: TransferJournalState::Skipped,
                attempt: 0,
                error: None,
            })?;
            skipped.push(download.key);
            continue;
        }
        let provider = Arc::clone(&provider);
        let gate = Arc::clone(&gate);
        let limiter = Arc::clone(&limiter);
        let journal = journal.clone();
        let max_attempts = tuning.max_attempts;
        tasks.spawn(async move {
            let _permit = limiter.acquire().await?;
            let key = download.key.clone();
            retry_transfer(
                &gate,
                Some(&limiter),
                max_attempts,
                TransferDirection::Download,
                &key,
                &journal,
                || async {
                    provider
                        .download(&download.key, &download.destination)
                        .await?;
                    Ok(download.key.clone())
                },
            )
            .await
            .map(|key| (index, key))
        });
    }

    let mut downloaded = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok(Ok(item)) => downloaded.push(item),
            Ok(Err(error)) => {
                tasks.abort_all();
                return Err(error);
            }
            Err(error) => {
                tasks.abort_all();
                return Err(StateError::Storage(format!(
                    "download worker failed: {error}"
                )));
            }
        }
    }
    downloaded.sort_by_key(|(index, _)| *index);
    Ok(DownloadBatchReport {
        downloaded: downloaded.into_iter().map(|(_, key)| key).collect(),
        skipped,
    })
}

async fn retry_transfer<T, F, Fut>(
    gate: &SharedRetryGate,
    limiter: Option<&AdaptiveConcurrency>,
    max_attempts: u32,
    direction: TransferDirection,
    key: &RemoteKey,
    journal: &TransferJournalCallbacks,
    mut operation: F,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    for attempt in 1..=max_attempts {
        gate.wait_for_turn().await;
        journal.record(&TransferJournalEvent {
            direction,
            key: key.clone(),
            state: TransferJournalState::Started,
            attempt,
            error: None,
        })?;
        let started = Instant::now();
        match operation().await {
            Ok(value) => {
                if let Some(limiter) = limiter {
                    limiter.record_success(started.elapsed());
                }
                journal.record(&TransferJournalEvent {
                    direction,
                    key: key.clone(),
                    state: TransferJournalState::Succeeded,
                    attempt,
                    error: None,
                })?;
                return Ok(value);
            }
            Err(error) => {
                let message = error.to_string();
                let class = classify_remote_error(None, &message);
                if let Some(limiter) = limiter {
                    limiter.record_error(class);
                }
                if class.is_retryable() && attempt < max_attempts {
                    gate.wait_for_retry(attempt, class).await;
                    continue;
                }
                journal.record(&TransferJournalEvent {
                    direction,
                    key: key.clone(),
                    state: TransferJournalState::Failed,
                    attempt,
                    error: Some(message),
                })?;
                return Err(error);
            }
        }
    }
    unreachable!("normalized retry count is always at least one")
}

pub fn local_storage_identity(root: &std::path::Path) -> ProviderRootIdentity {
    ProviderRootIdentity::new("local", "local", "local", root.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Health, LocalStorage, RemoteEntry};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;

    #[tokio::test]
    async fn compares_a_prefix_once_and_detects_size_conflicts() {
        let root = std::env::temp_dir().join(format!("noland-known-{}", uuid::Uuid::new_v4()));
        let source = root.join("source");
        let remote = root.join("remote");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(remote.join("packs/aa")).unwrap();
        std::fs::write(source.join("known"), b"same").unwrap();
        std::fs::write(source.join("missing"), b"new").unwrap();
        std::fs::write(source.join("conflict"), b"different").unwrap();
        std::fs::write(remote.join("packs/aa/known"), b"same").unwrap();
        std::fs::write(remote.join("packs/aa/conflict"), b"old").unwrap();
        let provider = LocalStorage::new(&remote);
        let candidates = vec![
            ImmutableUpload::new(source.join("known"), RemoteKey::new("packs/aa/known")),
            ImmutableUpload::new(source.join("missing"), RemoteKey::new("packs/aa/missing")),
            ImmutableUpload::new(source.join("conflict"), RemoteKey::new("packs/aa/conflict")),
        ];

        let known = list_remote_known(&provider, &RemoteKey::new("packs"))
            .await
            .unwrap();
        assert_eq!(known.len(), 2);
        assert!(known.contains(&RemoteKey::new("packs/aa/known")));
        let comparison = known.compare(&candidates).unwrap();
        assert_eq!(comparison.known.len(), 1);
        assert_eq!(comparison.missing.len(), 1);
        assert_eq!(comparison.size_conflicts.len(), 1);
        let _ = std::fs::remove_dir_all(root);
    }

    struct ConcurrencyProvider {
        active: AtomicUsize,
        peak: AtomicUsize,
    }

    #[async_trait]
    impl SharedStorageProvider for ConcurrencyProvider {
        async fn health_check(&self) -> Result<Health> {
            unreachable!()
        }
        async fn ensure_root(&self) -> Result<()> {
            Ok(())
        }
        async fn stat(&self, _key: &RemoteKey) -> Result<Option<RemoteMeta>> {
            unreachable!()
        }
        async fn upload_immutable(
            &self,
            local: &std::path::Path,
            key: &RemoteKey,
        ) -> Result<RemoteMeta> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(active, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(20)).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(RemoteMeta {
                key: key.clone(),
                size: std::fs::metadata(local)?.len(),
            })
        }
        async fn download(&self, _key: &RemoteKey, _dest: &std::path::Path) -> Result<()> {
            unreachable!()
        }
        async fn list_prefix(&self, _prefix: &RemoteKey) -> Result<Vec<RemoteEntry>> {
            unreachable!()
        }
        async fn put_small_versioned(&self, _bytes: Bytes, _key: &RemoteKey) -> Result<RemoteMeta> {
            unreachable!()
        }
    }

    struct MetadataRecordingProvider {
        keys: StdMutex<Vec<RemoteKey>>,
    }

    #[async_trait]
    impl SharedStorageProvider for MetadataRecordingProvider {
        async fn health_check(&self) -> Result<Health> {
            unreachable!()
        }
        async fn ensure_root(&self) -> Result<()> {
            Ok(())
        }
        async fn stat(&self, _key: &RemoteKey) -> Result<Option<RemoteMeta>> {
            unreachable!()
        }
        async fn upload_immutable(
            &self,
            _local: &std::path::Path,
            _key: &RemoteKey,
        ) -> Result<RemoteMeta> {
            unreachable!()
        }
        async fn download(&self, _key: &RemoteKey, _dest: &std::path::Path) -> Result<()> {
            unreachable!()
        }
        async fn list_prefix(&self, _prefix: &RemoteKey) -> Result<Vec<RemoteEntry>> {
            unreachable!()
        }
        async fn put_small_versioned(&self, bytes: Bytes, key: &RemoteKey) -> Result<RemoteMeta> {
            self.keys.lock().unwrap().push(key.clone());
            Ok(RemoteMeta {
                key: key.clone(),
                size: bytes.len() as u64,
            })
        }
    }

    #[tokio::test]
    async fn default_metadata_batch_writes_committed_last() {
        let provider = MetadataRecordingProvider {
            keys: StdMutex::new(Vec::new()),
        };
        let batch = MetadataBatch::with_committed(
            vec![
                MetadataWrite::new(
                    RemoteKey::new("bundle/index.enc"),
                    Bytes::from_static(b"index"),
                ),
                MetadataWrite::new(
                    RemoteKey::new("bundle/manifest.enc"),
                    Bytes::from_static(b"manifest"),
                ),
            ],
            MetadataWrite::new(
                RemoteKey::new("bundle/COMMITTED"),
                Bytes::from_static(b"commit"),
            ),
        );

        provider.put_metadata_batch(&batch).await.unwrap();
        assert_eq!(
            *provider.keys.lock().unwrap(),
            vec![
                RemoteKey::new("bundle/index.enc"),
                RemoteKey::new("bundle/manifest.enc"),
                RemoteKey::new("bundle/COMMITTED"),
            ]
        );
    }

    #[tokio::test]
    async fn bounded_downloads_honor_resume_callback() {
        let root = std::env::temp_dir().join(format!("noland-download-{}", uuid::Uuid::new_v4()));
        let remote = root.join("remote");
        let destination = root.join("destination");
        std::fs::create_dir_all(remote.join("packs")).unwrap();
        std::fs::write(remote.join("packs/one"), b"one").unwrap();
        std::fs::write(remote.join("packs/two"), b"two").unwrap();
        let provider = Arc::new(LocalStorage::new(&remote));
        let downloads = vec![
            DownloadRequest::new(RemoteKey::new("packs/one"), destination.join("one")),
            DownloadRequest::new(RemoteKey::new("packs/two"), destination.join("two")),
        ];
        let journal = TransferJournalCallbacks::new()
            .with_should_transfer(|_, key| Ok(key.as_str() != "packs/two"));

        let report = download_bounded(provider, downloads, TransferTuning::default(), journal)
            .await
            .unwrap();
        assert_eq!(report.downloaded, vec![RemoteKey::new("packs/one")]);
        assert_eq!(report.skipped, vec![RemoteKey::new("packs/two")]);
        assert_eq!(std::fs::read(destination.join("one")).unwrap(), b"one");
        assert!(!destination.join("two").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn bounded_uploads_honor_limit_and_resume_callback() {
        let root = std::env::temp_dir().join(format!("noland-bounded-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let mut uploads = Vec::new();
        for index in 0..5 {
            let path = root.join(index.to_string());
            std::fs::write(&path, b"x").unwrap();
            uploads.push(ImmutableUpload::new(
                path,
                RemoteKey::new(format!("packs/{index}")),
            ));
        }
        let provider = Arc::new(ConcurrencyProvider {
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        });
        let journal = TransferJournalCallbacks::new()
            .with_should_transfer(|_, key| Ok(key.as_str() != "packs/0"));
        let mut tuning = TransferTuning::default();
        tuning.max_parallel_uploads = 2;
        tuning.min_request_interval_ms = 0;

        let report = upload_immutable_bounded(Arc::clone(&provider), uploads, tuning, journal)
            .await
            .unwrap();
        assert_eq!(report.uploaded.len(), 4);
        assert_eq!(report.skipped, vec![RemoteKey::new("packs/0")]);
        assert_eq!(provider.peak.load(Ordering::SeqCst), 2);
        let _ = std::fs::remove_dir_all(root);
    }
}
