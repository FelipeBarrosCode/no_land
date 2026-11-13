use std::path::{Path, PathBuf};

use async_trait::async_trait;
use bytes::Bytes;
use noland_state_core::{Result, StateError};

use crate::{
    transfer::local_storage_identity, DownloadObserver, DownloadStreamConsumer, Health,
    ProviderRootIdentity, RemoteEntry, RemoteKey, RemoteMeta, SharedStorageProvider,
};

/// Filesystem-backed provider used by tests and as the local cache layout.
pub struct LocalStorage {
    pub root: PathBuf,
}

impl LocalStorage {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn abs(&self, key: &RemoteKey) -> PathBuf {
        self.root.join(key.as_str())
    }
}

struct UploadTemp(PathBuf);
impl Drop for UploadTemp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn identical_files(left: &Path, right: &Path) -> Result<bool> {
    use std::io::Read;
    if std::fs::metadata(left)?.len() != std::fs::metadata(right)?.len() {
        return Ok(false);
    }
    let mut left = std::fs::File::open(left)?;
    let mut right = std::fs::File::open(right)?;
    let mut a = [0u8; 64 * 1024];
    let mut b = [0u8; 64 * 1024];
    loop {
        let count = left.read(&mut a)?;
        if count == 0 {
            return Ok(right.read(&mut b[..1])? == 0);
        }
        right.read_exact(&mut b[..count])?;
        if a[..count] != b[..count] {
            return Ok(false);
        }
    }
}

#[async_trait]
impl SharedStorageProvider for LocalStorage {
    async fn remove_empty_bundle(
        &self,
        app: &noland_state_core::AppId,
        bundle: uuid::Uuid,
    ) -> Result<()> {
        let path = self.abs(&RemoteKey::new(noland_state_core::bundle_dir(app, bundle)));
        match noland_snapshot::SourcePath::new(&path)
            .and_then(|source| source.remove_empty_directory())
        {
            Ok(()) => Ok(()),
            Err(StateError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
    async fn delete_object(&self, key: &RemoteKey) -> Result<()> {
        crate::deletion::validate_deletable_key(key)?;
        let source = match noland_snapshot::SourcePath::new(&self.abs(key)) {
            Ok(source) => source,
            Err(StateError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(())
            }
            Err(error) => return Err(error),
        };
        let metadata = match source.metadata() {
            Ok(metadata) => metadata,
            Err(StateError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(())
            }
            Err(error) => return Err(error),
        };
        source.delete_leaf(&metadata)
    }

    fn supports_targeted_uploads(&self) -> bool {
        true
    }
    async fn health_check(&self) -> Result<Health> {
        Ok(Health {
            ok: true,
            provider: "local".into(),
            detail: self.root.display().to_string(),
        })
    }

    async fn ensure_root(&self) -> Result<()> {
        std::fs::create_dir_all(&self.root)?;
        for child in [
            "catalog",
            "commits",
            "bundles",
            "objects",
            "packs",
            "checkpoints",
            "instances",
        ] {
            std::fs::create_dir_all(self.root.join(child))?;
        }
        Ok(())
    }

    async fn stat(&self, key: &RemoteKey) -> Result<Option<RemoteMeta>> {
        let path = self.abs(key);
        let metadata = match std::fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if metadata.file_type().is_symlink() {
            return Err(StateError::UnsafePath(
                "symlink in storage inventory".into(),
            ));
        }
        Ok(Some(RemoteMeta {
            key: key.clone(),
            size: metadata.len(),
        }))
    }

    async fn upload_immutable(&self, local: &Path, key: &RemoteKey) -> Result<RemoteMeta> {
        crate::transfer::validate_remote_key(key)?;
        let dest = self.abs(key);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temporary = UploadTemp(dest.with_extension(format!("upload-{}", uuid::Uuid::new_v4())));
        std::fs::copy(local, &temporary.0)?;
        std::fs::File::open(&temporary.0)?.sync_all()?;
        match std::fs::hard_link(&temporary.0, &dest) {
            Ok(()) => {
                if let Some(parent) = dest.parent() {
                    std::fs::File::open(parent)?.sync_all()?;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if !identical_files(local, &dest)? {
                    return Err(StateError::Conflict(format!(
                        "immutable remote object differs: {}",
                        key.as_str()
                    )));
                }
            }
            Err(error) => return Err(error.into()),
        }
        Ok(RemoteMeta {
            key: key.clone(),
            size: std::fs::metadata(dest)?.len(),
        })
    }

    async fn download(&self, key: &RemoteKey, dest: &Path) -> Result<()> {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(self.abs(key), dest)?;
        Ok(())
    }

    fn supports_streaming_downloads(&self) -> bool {
        true
    }

    async fn download_stream_observed(
        &self,
        key: &RemoteKey,
        _scratch: &Path,
        consumer: &dyn DownloadStreamConsumer,
        observer: Option<&(dyn DownloadObserver + '_)>,
    ) -> Result<()> {
        use std::io::Read;
        consumer.restart().await?;
        if let Some(observer) = observer {
            observer.transfer_started()?;
        }
        let mut source = std::fs::File::open(self.abs(key))?;
        let mut buffer = vec![0; 256 * 1024];
        let mut bytes = 0;
        let mut last = std::time::Instant::now();
        loop {
            let count = source.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            consumer.write(&buffer[..count]).await?;
            bytes += count as u64;
            if last.elapsed() >= std::time::Duration::from_millis(500) {
                if let Some(observer) = observer {
                    observer.transfer_progress(bytes)?;
                }
                last = std::time::Instant::now();
            }
        }
        if let Some(observer) = observer {
            observer.transfer_progress(bytes)?;
        }
        consumer.finish().await
    }

    async fn list_prefix(&self, prefix: &RemoteKey) -> Result<Vec<RemoteEntry>> {
        let dir = self.abs(prefix);
        let mut out = Vec::new();
        match std::fs::symlink_metadata(&dir) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(StateError::UnsafePath(
                    "storage inventory prefix is not a directory".into(),
                ))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(error) => return Err(error.into()),
        }
        fn walk(base: &Path, dir: &Path, out: &mut Vec<RemoteEntry>) -> Result<()> {
            for entry in std::fs::read_dir(dir)? {
                let entry = entry?;
                let path = entry.path();
                let rel = path
                    .strip_prefix(base)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                let kind = entry.file_type()?;
                if kind.is_symlink() {
                    return Err(StateError::UnsafePath(
                        "symlink in storage inventory".into(),
                    ));
                }
                if kind.is_dir() {
                    out.push(RemoteEntry {
                        key: RemoteKey(rel),
                        size: 0,
                        is_prefix: true,
                    });
                    walk(base, &path, out)?;
                } else {
                    out.push(RemoteEntry {
                        key: RemoteKey(rel),
                        size: entry.metadata()?.len(),
                        is_prefix: false,
                    });
                }
            }
            Ok(())
        }
        walk(&self.root, &dir, &mut out)?;
        Ok(out)
    }

    async fn put_small_versioned(&self, bytes: Bytes, key: &RemoteKey) -> Result<RemoteMeta> {
        use std::io::Write;
        crate::transfer::validate_remote_key(key)?;
        let dest = self.abs(key);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temporary = UploadTemp(dest.with_extension(format!("write-{}", uuid::Uuid::new_v4())));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary.0)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary.0, &dest)?;
        if let Some(parent) = dest.parent() {
            std::fs::File::open(parent)?.sync_all()?;
        }
        Ok(RemoteMeta {
            key: key.clone(),
            size: bytes.len() as u64,
        })
    }

    fn storage_identity(&self) -> Option<ProviderRootIdentity> {
        Some(local_storage_identity(&self.root))
    }
}
