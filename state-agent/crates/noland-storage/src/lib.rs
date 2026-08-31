//! Shared Storage providers. Backup uses copy/additive semantics only.

mod commit;
mod local;
mod rclone;

pub use commit::{
    commit_bundle, commit_bundle_with_index, commit_checkpoint, commit_seal, load_catalog,
    read_committed_manifest, read_pack_index, update_catalog_with_bundle, CatalogStore,
};
pub use local::LocalStorage;
pub use rclone::{shred_ephemeral_session, write_ephemeral_session, RcloneStorage};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
        if let Ok(path) = dir.join("auth.json").canonicalize() {
            let _ = std::fs::write(&path, vec![0u8; 64]);
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
