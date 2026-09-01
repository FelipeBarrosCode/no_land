//! Shared Storage providers. Backup uses copy/additive semantics only.

mod commit;
mod local;
mod rclone;
mod transfer;

pub use commit::{
    commit_bundle, commit_bundle_with_index, commit_checkpoint, commit_seal, load_catalog,
    read_committed_manifest, read_pack_index, update_catalog_with_bundle, CatalogStore,
};
pub use local::LocalStorage;
pub use noland_rclone_adapter::{
    classify_remote_error, ProviderRootIdentity, RemoteErrorClass, TransferTuning,
};
pub use rclone::{
    shred_all_ephemeral_sessions, shred_ephemeral_session, write_ephemeral_session,
    write_guarded_ephemeral_session, EphemeralSessionGuard, RcloneStorage,
};
pub use transfer::{
    compare_remote_known, download_bounded, list_remote_known, upload_immutable_bounded,
    DownloadBatchReport, DownloadRequest, ImmutableUpload, MetadataBatch, MetadataWrite,
    RemoteKnownComparison, RemoteKnownSet, RemoteSizeConflict, SharedRetryGate, TransferDirection,
    TransferJournalCallbacks, TransferJournalEvent, TransferJournalState, UploadBatchReport,
};

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use bytes::Bytes;
use noland_state_core::{Result, StateError};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RemoteKey(pub String);

impl RemoteKey {
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    pub fn join(&self, child: &str) -> Self {
        if self.0.is_empty() {
            Self(child.into())
        } else {
            Self(format!(
                "{}/{}",
                self.0.trim_end_matches('/'),
                child.trim_start_matches('/')
            ))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteMeta {
    pub key: RemoteKey,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteEntry {
    pub key: RemoteKey,
    pub size: u64,
    pub is_prefix: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageOperationMetrics {
    pub rclone_invocations: u64,
    pub remote_stat_calls: u64,
    pub remote_list_calls: u64,
    pub remote_mkdir_calls: u64,
    pub remote_upload_calls: u64,
    pub remote_download_calls: u64,
    pub bytes_uploaded: u64,
    pub bytes_downloaded: u64,
}

impl StorageOperationMetrics {
    pub fn saturating_sub(self, previous: Self) -> Self {
        Self {
            rclone_invocations: self
                .rclone_invocations
                .saturating_sub(previous.rclone_invocations),
            remote_stat_calls: self
                .remote_stat_calls
                .saturating_sub(previous.remote_stat_calls),
            remote_list_calls: self
                .remote_list_calls
                .saturating_sub(previous.remote_list_calls),
            remote_mkdir_calls: self
                .remote_mkdir_calls
                .saturating_sub(previous.remote_mkdir_calls),
            remote_upload_calls: self
                .remote_upload_calls
                .saturating_sub(previous.remote_upload_calls),
            remote_download_calls: self
                .remote_download_calls
                .saturating_sub(previous.remote_download_calls),
            bytes_uploaded: self.bytes_uploaded.saturating_sub(previous.bytes_uploaded),
            bytes_downloaded: self
                .bytes_downloaded
                .saturating_sub(previous.bytes_downloaded),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Health {
    pub ok: bool,
    pub provider: String,
    pub detail: String,
}

#[async_trait]
pub trait SharedStorageProvider: Send + Sync {
    async fn health_check(&self) -> Result<Health>;
    async fn ensure_root(&self) -> Result<()>;
    async fn stat(&self, key: &RemoteKey) -> Result<Option<RemoteMeta>>;
    async fn upload_immutable(&self, local: &Path, key: &RemoteKey) -> Result<RemoteMeta>;
    async fn download(&self, key: &RemoteKey, dest: &Path) -> Result<()>;
    async fn list_prefix(&self, prefix: &RemoteKey) -> Result<Vec<RemoteEntry>>;
    async fn put_small_versioned(&self, bytes: Bytes, key: &RemoteKey) -> Result<RemoteMeta>;

    async fn upload_immutable_bulk(&self, uploads: &[ImmutableUpload]) -> Result<Vec<RemoteMeta>> {
        let mut uploaded = Vec::with_capacity(uploads.len());
        for upload in uploads {
            uploaded.push(self.upload_immutable(&upload.local, &upload.key).await?);
        }
        Ok(uploaded)
    }

    /// Writes ordinary metadata first and the visibility marker last.
    async fn put_metadata_batch(&self, batch: &MetadataBatch) -> Result<Vec<RemoteMeta>> {
        let mut written = Vec::with_capacity(batch.total_len());
        for entry in batch.entries() {
            written.push(
                self.put_small_versioned(entry.bytes.clone(), &entry.key)
                    .await?,
            );
        }
        if let Some(committed) = batch.committed() {
            written.push(
                self.put_small_versioned(committed.bytes.clone(), &committed.key)
                    .await?,
            );
        }
        Ok(written)
    }

    fn storage_identity(&self) -> Option<ProviderRootIdentity> {
        None
    }

    fn operation_metrics(&self) -> StorageOperationMetrics {
        StorageOperationMetrics::default()
    }
}

pub use noland_rclone_adapter::EphemeralRcloneSession;

/// Legacy OAuth-only handoff. Prefer [`EphemeralRcloneSession`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EphemeralStorageAuth {
    pub operation_id: String,
    pub provider: String,
    pub access_token: String,
    pub expires_at_unix: i64,
    pub root_name: String,
    pub rclone_remote: Option<String>,
}

impl Drop for EphemeralStorageAuth {
    fn drop(&mut self) {
        self.access_token.clear();
    }
}

pub fn write_ephemeral_auth(run_root: &Path, auth: &EphemeralStorageAuth) -> Result<PathBuf> {
    let dir = run_root.join("storage").join(&auth.operation_id);
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    let path = dir.join("auth.json");
    std::fs::write(&path, serde_json::to_vec(auth)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(path)
}

pub fn shred_ephemeral_auth(run_root: &Path, operation_id: &str) -> Result<()> {
    let dir = run_root.join("storage").join(operation_id);
    if dir.exists() {
        for filename in ["auth.json", "rclone.conf", "session.json"] {
            if let Ok(path) = dir.join(filename).canonicalize() {
                let len = std::fs::metadata(&path)
                    .map(|metadata| metadata.len().min(1024 * 1024) as usize)
                    .unwrap_or(64);
                let _ = std::fs::write(&path, vec![0u8; len]);
            }
        }
        std::fs::remove_dir_all(dir)?;
    }
    Ok(())
}

pub fn forbid_rclone_sync(args: &[String]) -> Result<()> {
    if args.iter().any(|a| a == "sync" || a == "bisync") {
        return Err(StateError::Storage(
            "rclone sync is forbidden for backup; use copy".into(),
        ));
    }
    Ok(())
}

pub const DEFAULT_ROOT: &str = noland_state_core::constants::DEFAULT_SHARED_STORAGE_ROOT;
