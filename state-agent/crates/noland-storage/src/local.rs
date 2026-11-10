use std::path::{Path, PathBuf};

use async_trait::async_trait;
use bytes::Bytes;
use noland_state_core::Result;

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

#[async_trait]
impl SharedStorageProvider for LocalStorage {
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
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(RemoteMeta {
            key: key.clone(),
            size: std::fs::metadata(path)?.len(),
        }))
    }

    async fn upload_immutable(&self, local: &Path, key: &RemoteKey) -> Result<RemoteMeta> {
        let dest = self.abs(key);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if dest.exists() {
            return Ok(RemoteMeta {
                key: key.clone(),
                size: std::fs::metadata(&dest)?.len(),
            });
        }
        std::fs::copy(local, &dest)?;
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
        consumer.restart()?;
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
            consumer.write(&buffer[..count])?;
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
        consumer.finish()
    }

    async fn list_prefix(&self, prefix: &RemoteKey) -> Result<Vec<RemoteEntry>> {
        let dir = self.abs(prefix);
        let mut out = Vec::new();
        if !dir.exists() {
            return Ok(out);
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
                if path.is_dir() {
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
        let dest = self.abs(key);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, &bytes)?;
        Ok(RemoteMeta {
            key: key.clone(),
            size: bytes.len() as u64,
        })
    }

    fn storage_identity(&self) -> Option<ProviderRootIdentity> {
        Some(local_storage_identity(&self.root))
    }
}
