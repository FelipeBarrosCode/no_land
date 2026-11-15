use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use noland_state_core::{Result, StateError};
use noland_storage::*;
use parking_lot::Mutex;
use tokio::sync::Barrier;

pub(crate) fn root(label: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/upload-review")
        .join(format!("index-{label}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::canonicalize(root).unwrap()
}

#[derive(Default)]
pub(crate) struct Controls {
    pub(crate) reads: Mutex<Vec<String>>,
    pub(crate) fail_reads: AtomicBool,
    pub(crate) fail_stats: AtomicBool,
    pub(crate) fail_pending: AtomicBool,
}

pub(crate) struct TestProvider {
    pub(crate) inner: LocalStorage,
    pub(crate) controls: Arc<Controls>,
    pub(crate) overlap: Option<Arc<Barrier>>,
}

impl Clone for TestProvider {
    fn clone(&self) -> Self {
        Self {
            inner: LocalStorage::new(self.inner.root.clone()),
            controls: self.controls.clone(),
            overlap: self.overlap.clone(),
        }
    }
}

impl TestProvider {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            inner: LocalStorage::new(root),
            controls: Arc::default(),
            overlap: None,
        }
    }

    pub(crate) fn reads(&self, key: &str) -> usize {
        self.controls
            .reads
            .lock()
            .iter()
            .filter(|entry| *entry == key)
            .count()
    }
}

pub(crate) fn session(root: &Path) -> EphemeralRcloneSession {
    EphemeralRcloneSession {
        operation_id: format!("index-{}", uuid::Uuid::new_v4()),
        provider: "local".into(),
        backend_type: "local".into(),
        remote_name: "fixture".into(),
        root: root.to_string_lossy().into_owned(),
        config_ini: "[fixture]\ntype = local\n".into(),
        expires_at_unix: 0,
        upload_concurrency: None,
    }
}

pub(crate) async fn write_catalog(
    provider: &dyn SharedStorageProvider,
    master: &noland_crypto::MasterKey,
    catalog: &noland_state_core::CatalogDocument,
) {
    let id = catalog.catalog_commit_id;
    let sealed = seal_metadata(
        &noland_crypto::derive_keys(master).catalog,
        b"catalog",
        catalog,
    )
    .unwrap();
    provider
        .put_metadata_batch(&MetadataBatch::with_committed(
            vec![MetadataWrite::new(
                RemoteKey::new(format!("catalog/commits/{id}.enc")),
                sealed.envelope,
            )],
            MetadataWrite::new(
                RemoteKey::new(format!("catalog/commits/{id}.COMMITTED")),
                bytes::Bytes::from_static(b"COMMITTED"),
            ),
        ))
        .await
        .unwrap();
    provider
        .put_small_versioned(id.to_string().into(), &RemoteKey::new("catalog/LATEST"))
        .await
        .unwrap();
}

#[async_trait]
impl SharedStorageProvider for TestProvider {
    async fn health_check(&self) -> Result<Health> {
        self.inner.health_check().await
    }
    async fn ensure_root(&self) -> Result<()> {
        self.inner.ensure_root().await
    }
    async fn stat(&self, key: &RemoteKey) -> Result<Option<RemoteMeta>> {
        if self.controls.fail_stats.load(Ordering::Relaxed) {
            return Err(StateError::Storage("fixture stat transport failure".into()));
        }
        self.inner.stat(key).await
    }
    async fn read_small(&self, key: &RemoteKey, limit: u64) -> Result<Vec<u8>> {
        self.controls.reads.lock().push(key.as_str().into());
        if self.controls.fail_reads.load(Ordering::Relaxed) {
            return Err(StateError::Storage(
                "fixture metadata transport failure".into(),
            ));
        }
        if key.as_str() == "catalog/LATEST" {
            if let Some(barrier) = &self.overlap {
                barrier.wait().await;
            }
        }
        self.inner.read_small(key, limit).await
    }
    async fn upload_immutable(&self, local: &Path, key: &RemoteKey) -> Result<RemoteMeta> {
        self.inner.upload_immutable(local, key).await
    }
    async fn download(&self, key: &RemoteKey, dest: &Path) -> Result<()> {
        self.controls.reads.lock().push(key.as_str().into());
        if self.controls.fail_reads.load(Ordering::Relaxed) {
            return Err(StateError::Storage(
                "fixture download transport failure".into(),
            ));
        }
        self.inner.download(key, dest).await
    }
    async fn list_prefix(&self, key: &RemoteKey) -> Result<Vec<RemoteEntry>> {
        if key.as_str() == "maintenance/pending" {
            if self.controls.fail_pending.load(Ordering::Relaxed) {
                return Err(StateError::Storage(
                    "fixture pending deletion transport failure".into(),
                ));
            }
            if let Some(barrier) = &self.overlap {
                barrier.wait().await;
            }
        }
        self.inner.list_prefix(key).await
    }
    async fn put_small_versioned(
        &self,
        bytes: bytes::Bytes,
        key: &RemoteKey,
    ) -> Result<RemoteMeta> {
        self.inner.put_small_versioned(bytes, key).await
    }
    async fn delete_object(&self, key: &RemoteKey) -> Result<()> {
        self.inner.delete_object(key).await
    }
    fn storage_identity(&self) -> Option<ProviderRootIdentity> {
        self.inner.storage_identity()
    }
}
