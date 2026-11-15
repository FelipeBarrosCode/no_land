//! A verification observation, never a cached path-safety decision. ctime is
//! retained until commit to catch in-place edits even when mtime is restored.
use std::fs;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

use noland_state_core::{ManifestFile, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct VerifiedIdentity {
    pub device: u64,
    pub inode: u64,
    pub size: u64,
    pub mode: u32,
    pub mtime_ns: i64,
    ctime_ns: i64,
    links: u64,
}

impl VerifiedIdentity {
    // libc stat field widths differ across Darwin, Linux, and 32-bit targets.
    #[allow(clippy::unnecessary_cast)]
    pub fn from_stat(metadata: &libc::stat) -> Self {
        Self {
            device: metadata.st_dev as u64,
            inode: metadata.st_ino as u64,
            size: metadata.st_size as u64,
            mode: metadata.st_mode as u32,
            mtime_ns: (metadata.st_mtime as i64)
                .saturating_mul(1_000_000_000)
                .saturating_add(metadata.st_mtime_nsec as i64),
            ctime_ns: (metadata.st_ctime as i64)
                .saturating_mul(1_000_000_000)
                .saturating_add(metadata.st_ctime_nsec as i64),
            links: metadata.st_nlink as u64,
        }
    }

    #[allow(clippy::unnecessary_cast)] // mode_t is u16 on Darwin and u32 on Linux.
    pub fn is_directory(self) -> bool {
        self.mode & libc::S_IFMT as u32 == libc::S_IFDIR as u32
    }

    pub fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.len(),
            mode: metadata.mode(),
            mtime_ns: metadata
                .mtime()
                .saturating_mul(1_000_000_000)
                .saturating_add(metadata.mtime_nsec()),
            ctime_ns: metadata
                .ctime()
                .saturating_mul(1_000_000_000)
                .saturating_add(metadata.ctime_nsec()),
            links: metadata.nlink(),
        }
    }

    pub fn with_mode(self, mode: u32) -> Self {
        Self { mode, ..self }
    }

    /// rename may change ctime. All content-related metadata must still match,
    /// and the post-rename observation becomes the new commit-time guard.
    pub fn matches_after_rename(self, current: Self) -> bool {
        Self {
            ctime_ns: current.ctime_ns,
            ..self
        } == current
    }

    pub fn matches_path(self, path: &Path) -> bool {
        fs::symlink_metadata(path)
            .is_ok_and(|metadata| metadata.is_file() && Self::from_metadata(&metadata) == self)
    }
}

pub(crate) fn verify_existing(
    path: &Path,
    manifest: &ManifestFile,
    metadata: Option<&fs::Metadata>,
) -> Result<Option<VerifiedIdentity>> {
    verify_contents(
        path,
        &manifest.file_hash,
        manifest.size,
        manifest.mode,
        metadata,
    )
}

pub(crate) fn verify_contents(
    path: &Path,
    hash: &str,
    size: u64,
    mode: Option<u32>,
    metadata: Option<&fs::Metadata>,
) -> Result<Option<VerifiedIdentity>> {
    if !metadata.is_some_and(|metadata| {
        metadata.is_file()
            && metadata.len() == size
            && mode.is_none_or(|mode| metadata.mode() & 0o7777 == mode & 0o7777)
    }) {
        return Ok(None);
    }
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let before = VerifiedIdentity::from_metadata(&file.metadata()?);
    if Some(before) != metadata.map(VerifiedIdentity::from_metadata) {
        return Ok(None);
    }
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 256 * 1024];
    loop {
        use std::io::Read;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let after = VerifiedIdentity::from_metadata(&file.metadata()?);
    Ok((before == after
        && after.matches_path(path)
        && format!("blake3:{}", hasher.finalize().to_hex()) == hash)
        .then_some(after))
}
