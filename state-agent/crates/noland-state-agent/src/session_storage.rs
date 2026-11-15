//! Agent-local identity adapter. All transfer/scheduling methods delegate
//! unchanged; only immutable catalog ciphertext can be served from a hint.
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use async_trait::async_trait;
use noland_crypto::{derive_keys, MasterKey};
use noland_rclone_adapter::{EphemeralRcloneSession, ProviderRootIdentity};
use noland_state_core::{AppId, CatalogDocument, Result, StateError};
use noland_storage::*;
use uuid::Uuid;

use crate::StateAgent;

#[cfg(test)]
mod tests;

pub(crate) struct SessionStorage<'a, P> {
    inner: P,
    agent: &'a StateAgent,
    master: &'a MasterKey,
    identity: ProviderRootIdentity,
    binding: String,
    stable_namespace: bool,
    hits: AtomicU64,
    misses: AtomicU64,
    cache_us: AtomicU64,
}

impl<'a, P> SessionStorage<'a, P> {
    pub(crate) fn new(
        inner: P,
        agent: &'a StateAgent,
        session: &EphemeralRcloneSession,
        master: &'a MasterKey,
    ) -> Self {
        let identity = crate::storage_scope::bound_session_identity(session, master);
        let stable_namespace = identity.remote_name.starts_with("namespace-v2-");
        let binding = crate::storage_scope::binding(Some(identity.clone()), master)
            .expect("bound session has an identity");
        Self {
            inner,
            agent,
            master,
            identity,
            binding,
            stable_namespace,
            hits: 0.into(),
            misses: 0.into(),
            cache_us: 0.into(),
        }
    }

    pub(crate) fn metadata_metrics(&self) -> serde_json::Value {
        serde_json::json!({
            "immutable_hits": self.hits.load(Ordering::Relaxed),
            "immutable_misses": self.misses.load(Ordering::Relaxed),
            "local_cache_us": self.cache_us.load(Ordering::Relaxed),
            "scope_mode": if self.stable_namespace { "stable_namespace" } else { "exact_private_config" },
        })
    }

    fn receipt(&self, binding: &str, key: &RemoteKey, bytes: &[u8]) -> String {
        let mut hash = blake3::Hasher::new_keyed(&self.master.0);
        hash.update(b"noland-immutable-catalog-v1\0");
        hash.update(binding.as_bytes());
        hash.update(&[0]);
        hash.update(key.as_str().as_bytes());
        hash.update(&[0]);
        hash.update(bytes);
        hash.finalize().to_hex().to_string()
    }
}

fn catalog_commit_id(key: &RemoteKey) -> Option<Uuid> {
    let id = key
        .as_str()
        .strip_prefix("catalog/commits/")?
        .strip_suffix(".enc")?;
    Uuid::parse_str(id)
        .ok()
        .filter(|parsed| parsed.to_string() == id)
}

#[async_trait]
impl<P: SharedStorageProvider> SharedStorageProvider for SessionStorage<'_, P> {
    async fn health_check(&self) -> Result<Health> {
        self.inner.health_check().await
    }
    async fn ensure_root(&self) -> Result<()> {
        self.inner.ensure_root().await
    }
    async fn stat(&self, key: &RemoteKey) -> Result<Option<RemoteMeta>> {
        self.inner.stat(key).await
    }
    async fn upload_immutable(&self, local: &Path, key: &RemoteKey) -> Result<RemoteMeta> {
        self.inner.upload_immutable(local, key).await
    }
    async fn download(&self, key: &RemoteKey, dest: &Path) -> Result<()> {
        self.inner.download(key, dest).await
    }
    async fn read_small(&self, key: &RemoteKey, limit: u64) -> Result<Vec<u8>> {
        if limit > MAX_SMALL_READ_BYTES {
            return Err(StateError::Invalid(
                "small read limit exceeds metadata bound".into(),
            ));
        }
        let Some(id) = catalog_commit_id(key) else {
            return self.inner.read_small(key, limit).await;
        };
        let binding = &self.binding;
        let started = Instant::now();
        let cached = self
            .agent
            .db
            .cached_metadata(binding, key.as_str())
            .ok()
            .flatten();
        self.cache_us.fetch_add(
            started.elapsed().as_micros().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
        if let Some(cached) = cached {
            let valid_receipt = blake3::Hash::from_hex(&cached.receipt).ok()
                == blake3::Hash::from_hex(self.receipt(binding, key, &cached.payload)).ok();
            if valid_receipt {
                // Never cache absence/existence or hide a transport error. Every
                // hit rechecks the current immutable object, in addition to
                // load_catalog's fresh LATEST/COMMITTED/tombstone checks.
                let current = self
                    .inner
                    .stat(key)
                    .await?
                    .ok_or_else(|| StateError::NotFound(key.as_str().into()))?;
                if current.size > limit {
                    return Err(StateError::Integrity(
                        "metadata envelope exceeds size limit".into(),
                    ));
                }
                if current.size == cached.payload.len() as u64 {
                    self.hits.fetch_add(1, Ordering::Relaxed);
                    return Ok(cached.payload);
                }
            }
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        let bytes = self.inner.read_small(key, limit).await?;
        let started = Instant::now();
        // Cache only small immutable catalogs; no whole-index memory duplication.
        if bytes.len() <= 8 * 1024 * 1024 {
            let catalog: CatalogDocument =
                open_metadata(&derive_keys(self.master).catalog, b"catalog", bytes.clone())?;
            if catalog.catalog_commit_id != id {
                return Err(StateError::Integrity("catalog commit ID mismatch".into()));
            }
            let receipt = self.receipt(binding, key, &bytes);
            if let Err(error) =
                self.agent
                    .db
                    .cache_authenticated_metadata(binding, key.as_str(), &bytes, &receipt)
            {
                tracing::warn!(%error, "immutable catalog cache unavailable; remote read succeeded");
            }
        }
        self.cache_us.fetch_add(
            started.elapsed().as_micros().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
        Ok(bytes)
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
    fn supports_streaming_downloads(&self) -> bool {
        self.inner.supports_streaming_downloads()
    }
    async fn download_stream_observed(
        &self,
        key: &RemoteKey,
        scratch: &Path,
        consumer: &dyn DownloadStreamConsumer,
        observer: Option<&(dyn DownloadObserver + '_)>,
    ) -> Result<()> {
        self.inner
            .download_stream_observed(key, scratch, consumer, observer)
            .await
    }
    async fn download_observed(
        &self,
        key: &RemoteKey,
        dest: &Path,
        observer: Option<&(dyn DownloadObserver + '_)>,
    ) -> Result<()> {
        self.inner.download_observed(key, dest, observer).await
    }
    async fn check_full_deletion_support(&self) -> Result<()> {
        self.inner.check_full_deletion_support().await
    }
    async fn delete_object(&self, key: &RemoteKey) -> Result<()> {
        self.inner.delete_object(key).await
    }
    async fn delete_prevalidated(&self, object: &PrevalidatedDeletion) -> Result<()> {
        self.inner.delete_prevalidated(object).await
    }
    async fn stat_for_deletion(&self, key: &RemoteKey) -> Result<Option<RemoteMeta>> {
        self.inner.stat_for_deletion(key).await
    }
    fn deletion_concurrency(&self) -> usize {
        self.inner.deletion_concurrency()
    }
    async fn remove_empty_bundle(&self, app: &AppId, bundle: Uuid) -> Result<()> {
        self.inner.remove_empty_bundle(app, bundle).await
    }
    async fn upload_immutable_bulk(&self, uploads: &[ImmutableUpload]) -> Result<Vec<RemoteMeta>> {
        self.inner.upload_immutable_bulk(uploads).await
    }
    async fn upload_immutable_bulk_observed(
        &self,
        uploads: &[ImmutableUpload],
        observer: Option<&(dyn ImmutableUploadObserver + '_)>,
    ) -> Result<Vec<RemoteMeta>> {
        self.inner
            .upload_immutable_bulk_observed(uploads, observer)
            .await
    }
    async fn upload_immutable_bulk_known_observed(
        &self,
        uploads: &[ImmutableUpload],
        known: &RemoteKnownSet,
        observer: Option<&(dyn ImmutableUploadObserver + '_)>,
    ) -> Result<Vec<RemoteMeta>> {
        self.inner
            .upload_immutable_bulk_known_observed(uploads, known, observer)
            .await
    }
    fn supports_parallel_upload_groups(&self) -> bool {
        self.inner.supports_parallel_upload_groups()
    }
    fn supports_targeted_uploads(&self) -> bool {
        self.inner.supports_targeted_uploads()
    }
    fn transfer_tuning(&self) -> TransferTuning {
        self.inner.transfer_tuning()
    }
    async fn put_metadata_batch(&self, batch: &MetadataBatch) -> Result<Vec<RemoteMeta>> {
        self.inner.put_metadata_batch(batch).await
    }
    fn provider_kind(&self) -> Option<ProviderKind> {
        self.inner.provider_kind()
    }
    fn capabilities(&self) -> Option<&ProviderCapabilities> {
        self.inner.capabilities()
    }
    fn storage_identity(&self) -> Option<ProviderRootIdentity> {
        Some(self.identity.clone())
    }
    fn operation_metrics(&self) -> StorageOperationMetrics {
        self.inner.operation_metrics()
    }
    fn metadata_read_metrics(&self) -> MetadataReadMetrics {
        self.inner.metadata_read_metrics()
    }
}
