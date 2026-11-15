//! Bounded, exact-object metadata reads. No repository-wide or negative cache:
//! deletion/admission records must always be read from this operation's remote.

use std::io::Read;
use std::path::{Path, PathBuf};

use noland_state_core::{Result, StateError};
use serde::{Deserialize, Serialize};

use crate::{RemoteKey, SharedStorageProvider};

pub const MAX_SMALL_READ_BYTES: u64 = crate::metadata::MAX_METADATA_ENVELOPE_BYTES;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct MetadataReadMetrics {
    pub read_calls: u64,
    pub attempts: u64,
    /// Payload read from stdout, including failed attempts and an oversize probe.
    /// This is not transport/protocol overhead or successfully decoded bytes.
    pub bytes_received: u64,
    /// Child lifetime including startup/remote/pipe/exit wait, excluding measured
    /// local buffer work. It is deliberately not claimed to be pure network time.
    pub process_wait_us: u64,
    pub local_buffer_us: u64,
    pub admission_wait_us: u64,
    pub retry_wait_us: u64,
}

impl MetadataReadMetrics {
    pub fn saturating_sub(self, previous: Self) -> Self {
        Self {
            read_calls: self.read_calls.saturating_sub(previous.read_calls),
            attempts: self.attempts.saturating_sub(previous.attempts),
            bytes_received: self.bytes_received.saturating_sub(previous.bytes_received),
            process_wait_us: self
                .process_wait_us
                .saturating_sub(previous.process_wait_us),
            local_buffer_us: self
                .local_buffer_us
                .saturating_sub(previous.local_buffer_us),
            admission_wait_us: self
                .admission_wait_us
                .saturating_sub(previous.admission_wait_us),
            retry_wait_us: self.retry_wait_us.saturating_sub(previous.retry_wait_us),
        }
    }
}

pub(crate) fn validate_small_read(key: &RemoteKey, limit: u64) -> Result<usize> {
    crate::transfer::validate_remote_key(key)?;
    if key.as_str().split('/').any(|part| {
        part.is_empty() || matches!(part, "." | "..") || part.contains(['\\', '\0', '\n', '\r'])
    }) {
        return Err(StateError::UnsafePath("noncanonical metadata key".into()));
    }
    if limit > MAX_SMALL_READ_BYTES {
        return Err(StateError::Invalid(
            "small read limit exceeds metadata bound".into(),
        ));
    }
    usize::try_from(limit)
        .map_err(|_| StateError::Invalid("small read limit exceeds addressable memory".into()))
}

pub(crate) fn limit_exceeded() -> StateError {
    StateError::Integrity("metadata envelope exceeds size limit".into())
}

pub(crate) fn absent() -> StateError {
    StateError::NotFound("metadata object is absent".into())
}

/// Keep capacity as well as length bounded; avoid a full-limit allocation for
/// tiny pointers, geometric Vec growth past the limit, or overflow at limit + 1.
pub(crate) struct SmallReadBuffer {
    bytes: Vec<u8>,
    limit: usize,
}

impl SmallReadBuffer {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }

    pub(crate) fn next_read_len(&self, buffer_len: usize) -> usize {
        // Validated limits are at most 513 MiB, including on 32-bit targets.
        buffer_len.min(self.limit - self.bytes.len() + 1)
    }

    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<()> {
        let length = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .filter(|length| *length <= self.limit)
            .ok_or_else(limit_exceeded)?;
        if length > self.bytes.capacity() {
            let capacity = self
                .bytes
                .capacity()
                .max(8192)
                .saturating_mul(2)
                .min(self.limit)
                .max(length);
            self.bytes
                .try_reserve_exact(capacity - self.bytes.len())
                .map_err(|_| {
                    StateError::Storage("unable to allocate bounded metadata buffer".into())
                })?;
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    pub(crate) fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

pub(crate) fn read_file_bounded(file: std::fs::File, limit: usize) -> Result<Vec<u8>> {
    if file.metadata()?.len() > limit as u64 {
        return Err(limit_exceeded());
    }
    let mut reader = file;
    let mut bytes = SmallReadBuffer::new(limit);
    let mut buffer = [0; 16 * 1024];
    loop {
        let count = bytes.next_read_len(buffer.len());
        let count = reader.read(&mut buffer[..count])?;
        if count == 0 {
            return Ok(bytes.finish());
        }
        bytes.push(&buffer[..count])?;
    }
}

#[cfg(unix)]
pub(crate) fn open_metadata_file(path: &Path) -> Result<std::fs::File> {
    noland_snapshot::open_regular_nofollow(path)
}

#[cfg(not(unix))]
pub(crate) fn open_metadata_file(path: &Path) -> Result<std::fs::File> {
    // Descriptor-anchored snapshot capture is Unix-only. Keep legacy storage
    // providers functional elsewhere, rejecting links/non-files at the leaf.
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(StateError::UnsafePath(
            "metadata is not a regular file".into(),
        ));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(StateError::UnsafePath("metadata is a reparse point".into()));
        }
    }
    if !metadata.is_file() {
        return Err(StateError::UnsafePath(
            "metadata is not a regular file".into(),
        ));
    }
    Ok(file)
}

struct MetadataTemp(PathBuf);

impl MetadataTemp {
    fn new() -> Result<Self> {
        // macOS's conventional /var temp path aliases /private/var. Resolve the
        // trusted temp root before using the no-follow file opener below.
        let path = std::env::temp_dir()
            .canonicalize()?
            .join(format!("noland-metadata-{}", uuid::Uuid::new_v4()));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path)?;
        Ok(Self(path))
    }
}

impl Drop for MetadataTemp {
    fn drop(&mut self) {
        // This exclusively-created directory contains only this read's scratch.
        let _ = std::fs::remove_file(self.0.join("object"));
        let _ = std::fs::remove_dir(&self.0);
    }
}

pub(crate) async fn read_small_fallback<P: SharedStorageProvider + ?Sized>(
    provider: &P,
    key: &RemoteKey,
    limit: u64,
) -> Result<Vec<u8>> {
    let bound = validate_small_read(key, limit)?;
    // Legacy download() does not promise classified remote absence. Preserve
    // its stat, rather than reclassifying arbitrary local IO/transport errors.
    let meta = provider.stat(key).await?.ok_or_else(absent)?;
    if meta.key != *key {
        return Err(StateError::Integrity(
            "metadata stat identity mismatch".into(),
        ));
    }
    if meta.size > limit {
        return Err(limit_exceeded());
    }
    let temporary = MetadataTemp::new()?;
    let destination = temporary.0.join("object");
    provider.download(key, &destination).await?;
    // Recheck the downloaded file, not the earlier stat; concurrent growth must
    // never return a valid-looking truncated metadata prefix.
    read_file_bounded(open_metadata_file(&destination)?, bound)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Health, LocalStorage, RemoteEntry, RemoteMeta};
    use async_trait::async_trait;
    use bytes::Bytes;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    };

    struct LegacyProvider {
        meta: Option<RemoteMeta>,
        bytes: Vec<u8>,
        stat_auth_error: bool,
        download_io_error: bool,
        block_download: bool,
        stats: AtomicUsize,
        downloads: AtomicUsize,
        scratch: Mutex<Option<PathBuf>>,
        entered: tokio::sync::Notify,
    }

    impl LegacyProvider {
        fn new(bytes: &[u8]) -> Self {
            Self {
                meta: Some(RemoteMeta {
                    key: RemoteKey::new("catalog/test"),
                    size: bytes.len() as u64,
                }),
                bytes: bytes.to_vec(),
                stat_auth_error: false,
                download_io_error: false,
                block_download: false,
                stats: AtomicUsize::new(0),
                downloads: AtomicUsize::new(0),
                scratch: Mutex::default(),
                entered: tokio::sync::Notify::new(),
            }
        }
    }

    #[async_trait]
    impl SharedStorageProvider for LegacyProvider {
        async fn health_check(&self) -> Result<Health> {
            unreachable!()
        }
        async fn ensure_root(&self) -> Result<()> {
            unreachable!()
        }
        async fn stat(&self, _: &RemoteKey) -> Result<Option<RemoteMeta>> {
            self.stats.fetch_add(1, Ordering::SeqCst);
            if self.stat_auth_error {
                return Err(StateError::Storage("authentication failed".into()));
            }
            Ok(self.meta.clone())
        }
        async fn download(&self, _: &RemoteKey, destination: &Path) -> Result<()> {
            self.downloads.fetch_add(1, Ordering::SeqCst);
            *self.scratch.lock().unwrap() = Some(destination.to_owned());
            if self.download_io_error {
                // Ambiguous legacy errors (e.g. a missing executable/local path)
                // are not automatically a missing remote object.
                return Err(std::io::Error::from(std::io::ErrorKind::NotFound).into());
            }
            std::fs::write(destination, &self.bytes)?;
            self.entered.notify_one();
            if self.block_download {
                std::future::pending::<()>().await;
            }
            Ok(())
        }
        async fn upload_immutable(&self, _: &Path, _: &RemoteKey) -> Result<RemoteMeta> {
            unreachable!()
        }
        async fn list_prefix(&self, _: &RemoteKey) -> Result<Vec<RemoteEntry>> {
            unreachable!()
        }
        async fn put_small_versioned(&self, _: Bytes, _: &RemoteKey) -> Result<RemoteMeta> {
            unreachable!()
        }
    }

    #[tokio::test]
    async fn legacy_fallback_bounds_growth_and_cleans_scratch_on_success_and_failure() {
        let key = RemoteKey::new("catalog/test");
        for (bytes, limit, reported_size, succeeds) in [
            (b"".as_slice(), 0, 0, true),
            (b"abc".as_slice(), 3, 3, true),
            (b"abcd".as_slice(), 3, 3, false), // grew after stat
            (b"x".as_slice(), 0, 0, false),
        ] {
            let mut provider = LegacyProvider::new(bytes);
            provider.meta.as_mut().unwrap().size = reported_size;
            let result = provider.read_small(&key, limit).await;
            if succeeds {
                assert_eq!(result.unwrap(), bytes);
            } else {
                assert!(matches!(result, Err(StateError::Integrity(_))));
            }
            let scratch = provider.scratch.lock().unwrap().clone().unwrap();
            assert!(!scratch.exists());
            assert!(!scratch.parent().unwrap().exists());
        }
    }

    #[tokio::test]
    async fn limits_and_wrong_stat_identity_fail_before_download_without_overflow() {
        let key = RemoteKey::new("catalog/test");
        let provider = LegacyProvider::new(b"abc");
        assert!(matches!(
            provider.read_small(&key, u64::MAX).await,
            Err(StateError::Invalid(_))
        ));
        assert_eq!(provider.stats.load(Ordering::SeqCst), 0);
        let mut oversized = LegacyProvider::new(b"abc");
        oversized.meta.as_mut().unwrap().size = u64::MAX;
        assert!(matches!(
            oversized.read_small(&key, 3).await,
            Err(StateError::Integrity(_))
        ));
        assert_eq!(oversized.downloads.load(Ordering::SeqCst), 0);
        let mut wrong_key = LegacyProvider::new(b"abc");
        wrong_key.meta.as_mut().unwrap().key = RemoteKey::new("catalog/wrong");
        assert!(matches!(
            wrong_key.read_small(&key, 3).await,
            Err(StateError::Integrity(_))
        ));
        assert_eq!(wrong_key.downloads.load(Ordering::SeqCst), 0);
        for key in [
            "catalog/../key",
            "catalog//key",
            "catalog/./key",
            "catalog/key\nother",
            "catalog/key\\other",
        ] {
            assert!(provider.read_small(&RemoteKey::new(key), 3).await.is_err());
        }
        assert_eq!(provider.stats.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn only_classified_absence_is_optional_not_auth_or_ambiguous_io() {
        let key = RemoteKey::new("catalog/test");
        let mut provider = LegacyProvider::new(b"abc");
        provider.meta = None;
        assert!(
            crate::commit::download_optional_metadata(&provider, &key, 3)
                .await
                .unwrap()
                .is_none()
        );
        provider.stat_auth_error = true;
        assert!(matches!(
            crate::commit::download_optional_metadata(&provider, &key, 3).await,
            Err(StateError::Storage(_))
        ));
        provider.stat_auth_error = false;
        provider.meta = Some(RemoteMeta {
            key: key.clone(),
            size: 3,
        });
        provider.download_io_error = true;
        assert!(matches!(
            crate::commit::download_optional_metadata(&provider, &key, 3).await,
            Err(StateError::Io(_))
        ));
        let scratch = provider.scratch.lock().unwrap().clone().unwrap();
        assert!(!scratch.parent().unwrap().exists());
    }

    #[tokio::test]
    async fn cancelling_legacy_read_removes_its_private_scratch() {
        let mut provider = LegacyProvider::new(b"abc");
        provider.block_download = true;
        let key = RemoteKey::new("catalog/test");
        let mut future = Box::pin(provider.read_small(&key, 3));
        tokio::select! {
            _ = provider.entered.notified() => {},
            _ = &mut future => panic!("fixture should remain in download"),
        }
        let scratch = provider.scratch.lock().unwrap().clone().unwrap();
        assert!(scratch.exists());
        drop(future);
        assert!(!scratch.exists());
        assert!(!scratch.parent().unwrap().exists());
    }

    #[test]
    fn buffer_capacity_and_overflow_probe_are_bounded_at_every_growth_boundary() {
        for limit in [0, 1, 8191, 8192, 16383, 16384, 16385, 65537] {
            let mut buffer = SmallReadBuffer::new(limit);
            while buffer.bytes.len() < limit {
                let count = (limit - buffer.bytes.len()).min(1031);
                buffer.push(&vec![b'x'; count]).unwrap();
                assert!(buffer.bytes.capacity() <= limit);
            }
            assert_eq!(buffer.next_read_len(16384), 1);
            assert!(matches!(buffer.push(b"x"), Err(StateError::Integrity(_))));
            assert_eq!(buffer.finish(), vec![b'x'; limit]);
        }
    }

    #[tokio::test]
    async fn local_small_reads_are_exact_bounded_and_do_not_follow_symlinks() {
        let root =
            std::env::temp_dir().join(format!("noland-small-local-{}", uuid::Uuid::new_v4()));
        let provider = LocalStorage::new(&root);
        provider.ensure_root().await.unwrap();
        let key = RemoteKey::new("catalog/test");
        assert!(matches!(
            provider.read_small(&key, 4).await,
            Err(StateError::NotFound(_))
        ));
        provider
            .put_small_versioned(Bytes::from_static(b"abcd"), &key)
            .await
            .unwrap();
        assert_eq!(provider.read_small(&key, 4).await.unwrap(), b"abcd");
        assert!(matches!(
            provider.read_small(&key, 3).await,
            Err(StateError::Integrity(_))
        ));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("catalog/test"), root.join("catalog/link"))
                .unwrap();
            assert!(matches!(
                provider
                    .read_small(&RemoteKey::new("catalog/link"), 4)
                    .await,
                Err(StateError::UnsafePath(_))
            ));
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn metadata_metrics_deltas_saturate_without_changing_operation_struct_literals() {
        let old = MetadataReadMetrics {
            read_calls: 10,
            attempts: 20,
            ..Default::default()
        };
        let new = MetadataReadMetrics {
            read_calls: 11,
            attempts: 2,
            bytes_received: 8,
            ..Default::default()
        };
        let delta = new.saturating_sub(old);
        assert_eq!(
            (delta.read_calls, delta.attempts, delta.bytes_received),
            (1, 0, 8)
        );
        let _compatible = crate::StorageOperationMetrics {
            rclone_invocations: 0,
            remote_stat_calls: 0,
            remote_list_calls: 0,
            remote_mkdir_calls: 0,
            remote_upload_calls: 0,
            remote_download_calls: 0,
            bytes_uploaded: 0,
            bytes_downloaded: 0,
        };
    }
}
