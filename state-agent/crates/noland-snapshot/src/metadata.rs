//! Common observations for descriptor metadata and no-follow `fstatat` metadata.
//! Rust's `fs::Metadata` cannot be constructed from `libc::stat`, so normalize
//! only the fields used by capture instead of opening a file just to call fstat.
use std::fs;
#[cfg(unix)]
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceMetadata {
    size: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    mode: u32,
    #[cfg(unix)]
    uid: u32,
    #[cfg(unix)]
    gid: u32,
    #[cfg(unix)]
    ctime: (i64, i64),
    #[cfg(not(unix))]
    kind: fs::FileType,
}

impl SourceMetadata {
    pub fn len(&self) -> u64 {
        self.size
    }
    pub fn is_empty(&self) -> bool {
        self.size == 0
    }

    pub fn mtime_ns(&self) -> Option<i64> {
        self.modified?
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
    }

    pub fn is_file(&self) -> bool {
        #[cfg(unix)]
        {
            self.mode & libc::S_IFMT as u32 == libc::S_IFREG as u32
        }
        #[cfg(not(unix))]
        {
            self.kind.is_file()
        }
    }

    pub fn is_dir(&self) -> bool {
        #[cfg(unix)]
        {
            self.mode & libc::S_IFMT as u32 == libc::S_IFDIR as u32
        }
        #[cfg(not(unix))]
        {
            self.kind.is_dir()
        }
    }

    pub fn is_symlink(&self) -> bool {
        #[cfg(unix)]
        {
            self.mode & libc::S_IFMT as u32 == libc::S_IFLNK as u32
        }
        #[cfg(not(unix))]
        {
            self.kind.is_symlink()
        }
    }

    #[cfg(unix)]
    pub fn dev(&self) -> u64 {
        self.device
    }
    #[cfg(unix)]
    pub fn ino(&self) -> u64 {
        self.inode
    }
    #[cfg(unix)]
    pub fn mode(&self) -> u32 {
        self.mode
    }
    #[cfg(unix)]
    pub fn uid(&self) -> u32 {
        self.uid
    }
    #[cfg(unix)]
    pub fn gid(&self) -> u32 {
        self.gid
    }

    #[cfg(unix)]
    #[allow(clippy::unnecessary_cast)] // libc field widths differ across targets.
    pub(crate) fn from_stat(metadata: &libc::stat) -> Self {
        Self {
            size: metadata.st_size as u64,
            modified: unix_time(metadata.st_mtime as i64, metadata.st_mtime_nsec as i64),
            device: metadata.st_dev as u64,
            inode: metadata.st_ino as u64,
            mode: metadata.st_mode as u32,
            uid: metadata.st_uid as u32,
            gid: metadata.st_gid as u32,
            ctime: (metadata.st_ctime as i64, metadata.st_ctime_nsec as i64),
        }
    }
}

#[cfg(unix)]
fn unix_time(seconds: i64, nanos: i64) -> Option<SystemTime> {
    if !(0..1_000_000_000).contains(&nanos) {
        return None;
    }
    let base = if seconds >= 0 {
        UNIX_EPOCH.checked_add(Duration::from_secs(seconds as u64))?
    } else {
        UNIX_EPOCH.checked_sub(Duration::from_secs(seconds.unsigned_abs()))?
    };
    base.checked_add(Duration::from_nanos(nanos as u64))
}

impl From<&fs::Metadata> for SourceMetadata {
    fn from(metadata: &fs::Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Self {
            size: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
            #[cfg(unix)]
            mode: metadata.mode(),
            #[cfg(unix)]
            uid: metadata.uid(),
            #[cfg(unix)]
            gid: metadata.gid(),
            #[cfg(unix)]
            ctime: (metadata.ctime(), metadata.ctime_nsec()),
            #[cfg(not(unix))]
            kind: metadata.file_type(),
        }
    }
}

impl From<&SourceMetadata> for SourceMetadata {
    fn from(metadata: &SourceMetadata) -> Self {
        metadata.clone()
    }
}

/// Accept both held-descriptor `fs::Metadata` and anchored `SourceMetadata`.
/// Keep type, device/inode, exact size/mtime/ctime and ownership/mode checks.
pub fn same_file_identity(
    before: impl Into<SourceMetadata>,
    after: impl Into<SourceMetadata>,
) -> bool {
    before.into() == after.into()
}
