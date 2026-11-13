//! Source names are resolved one component at a time, relative to held directory
//! descriptors. A rename can detach a directory, but cannot redirect a later open
//! through its replacement. Never canonicalize a source (that follows links).
use std::fs::File;
use std::path::{Path, PathBuf};

use crate::SourceMetadata;
use noland_state_core::{Result, StateError};

#[cfg(unix)]
mod unix {
    use super::*;
    use std::ffi::{CStr, CString, OsStr, OsString};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::path::Component;

    pub struct SourcePath {
        parent: File,
        name: OsString,
    }

    fn name(value: &OsStr) -> Result<CString> {
        CString::new(value.as_bytes())
            .map_err(|_| StateError::UnsafePath("NUL in capture path".into()))
    }

    fn open_at(parent: &File, leaf: &OsStr, flags: libc::c_int) -> Result<File> {
        let leaf = name(leaf)?;
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                leaf.as_ptr(),
                flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            let error = std::io::Error::last_os_error();
            if matches!(error.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) {
                return Err(StateError::UnsafePath(
                    "symlink or non-directory in capture path".into(),
                ));
            }
            return Err(error.into());
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    impl SourcePath {
        pub fn new(path: &Path) -> Result<Self> {
            let mut parent = File::open(if path.is_absolute() { "/" } else { "." })?;
            let mut components = Vec::new();
            for part in path.components() {
                match part {
                    Component::Normal(part) => components.push(part),
                    Component::RootDir | Component::CurDir => {}
                    _ => {
                        return Err(StateError::UnsafePath(format!(
                            "invalid capture path: {}",
                            path.display()
                        )))
                    }
                }
            }
            let leaf = components.pop().unwrap_or(OsStr::new("."));
            for part in components {
                parent = open_at(&parent, part, libc::O_RDONLY | libc::O_DIRECTORY)?;
            }
            Ok(Self {
                parent,
                name: leaf.to_owned(),
            })
        }

        pub fn open_regular(&self) -> Result<File> {
            // NONBLOCK prevents a replaced FIFO/device from blocking before fstat.
            let file = open_at(&self.parent, &self.name, libc::O_RDONLY | libc::O_NONBLOCK)?;
            if !file.metadata()?.is_file() {
                return Err(StateError::UnsafePath(
                    "capture source is not a regular file".into(),
                ));
            }
            Ok(file)
        }

        /// Private persistent lock file, opened relative to the held parent.
        /// Never follow a replacement symlink, truncate it, or open a device.
        pub fn open_or_create_private(&self) -> Result<File> {
            let leaf = name(&self.name)?;
            let fd = unsafe {
                libc::openat(
                    self.parent.as_raw_fd(),
                    leaf.as_ptr(),
                    libc::O_RDWR
                        | libc::O_CREAT
                        | libc::O_CLOEXEC
                        | libc::O_NOFOLLOW
                        | libc::O_NONBLOCK,
                    0o600,
                )
            };
            if fd < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            let file = unsafe { File::from_raw_fd(fd) };
            if !file.metadata()?.is_file() {
                return Err(StateError::UnsafePath(
                    "repository lock is not a regular file".into(),
                ));
            }
            Ok(file)
        }

        /// Observe the final entry without following it or opening its data.
        /// Content readers must still compare with metadata from their opened
        /// descriptor, since the entry can be replaced after this observation.
        pub fn metadata(&self) -> Result<SourceMetadata> {
            let leaf = name(&self.name)?;
            let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
            // No source open and no data-read permission: fstatat observes the
            // final entry itself, relative to the already anchored parent.
            if unsafe {
                libc::fstatat(
                    self.parent.as_raw_fd(),
                    leaf.as_ptr(),
                    metadata.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } != 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
            // A successful fstatat initialized the stat fields.
            let metadata = unsafe { metadata.assume_init() };
            Ok(SourceMetadata::from_stat(&metadata))
        }

        pub fn read_link(&self) -> Result<PathBuf> {
            let leaf = name(&self.name)?;
            let mut bytes = vec![0u8; 256];
            loop {
                let count = unsafe {
                    libc::readlinkat(
                        self.parent.as_raw_fd(),
                        leaf.as_ptr(),
                        bytes.as_mut_ptr().cast(),
                        bytes.len(),
                    )
                };
                if count < 0 {
                    return Err(std::io::Error::last_os_error().into());
                }
                if (count as usize) < bytes.len() {
                    bytes.truncate(count as usize);
                    return Ok(PathBuf::from(OsString::from_vec(bytes)));
                }
                if bytes.len() >= 64 * 1024 {
                    return Err(StateError::UnsafePath(
                        "capture symlink target is too long".into(),
                    ));
                }
                bytes.resize(bytes.len() * 2, 0);
            }
        }

        pub fn parent_identity(&self) -> Result<(u64, u64)> {
            use std::os::unix::fs::MetadataExt;
            let metadata = self.parent.metadata()?;
            Ok((metadata.dev(), metadata.ino()))
        }

        /// Empty-directory removal only; never follows or recursively sweeps a
        /// replacement. Kernel AT_REMOVEDIR refuses symlinks and nonempty trees.
        pub fn remove_empty_directory(&self) -> Result<()> {
            if !self.metadata()?.is_dir() {
                return Err(StateError::UnsafePath("expected an empty directory".into()));
            }
            let leaf = name(&self.name)?;
            if unsafe { libc::unlinkat(self.parent.as_raw_fd(), leaf.as_ptr(), libc::AT_REMOVEDIR) }
                != 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
            self.parent.sync_all()?;
            Ok(())
        }

        /// Move a leaf into a private, same-filesystem quarantine before unlink.
        /// A replacement racing the rename is retained there, never deleted.
        /// Directories and symlink traversal are deliberately unsupported.
        pub fn delete_leaf(&self, expected: &SourceMetadata) -> Result<()> {
            if !(expected.is_file() || expected.is_symlink()) || self.metadata()? != *expected {
                return Err(StateError::Conflict(
                    "file changed since deletion preview".into(),
                ));
            }
            let quarantine = OsString::from(format!(".noland-delete-{}", uuid::Uuid::new_v4()));
            let quarantine_c = name(&quarantine)?;
            if unsafe { libc::mkdirat(self.parent.as_raw_fd(), quarantine_c.as_ptr(), 0o700) } != 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
            let directory = open_at(
                &self.parent,
                &quarantine,
                libc::O_RDONLY | libc::O_DIRECTORY,
            )?;
            let source = name(&self.name)?;
            let entry = name(OsStr::new("entry"))?;
            if unsafe {
                libc::renameat(
                    self.parent.as_raw_fd(),
                    source.as_ptr(),
                    directory.as_raw_fd(),
                    entry.as_ptr(),
                )
            } != 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
            self.parent.sync_all()?;
            directory.sync_all()?;
            let observed = Self::child(&directory, OsString::from("entry"))?.metadata()?;
            // rename changes ctime; all other identity/content fields must agree.
            if observed.dev() != expected.dev()
                || observed.ino() != expected.ino()
                || observed.len() != expected.len()
                || observed.mtime_ns() != expected.mtime_ns()
                || observed.mode() != expected.mode()
                || observed.uid() != expected.uid()
                || observed.gid() != expected.gid()
            {
                return Err(StateError::Conflict(format!(
                    "raced deletion entry retained in {}/entry beside the source",
                    quarantine.to_string_lossy()
                )));
            }
            if unsafe { libc::unlinkat(directory.as_raw_fd(), entry.as_ptr(), 0) } != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            directory.sync_all()?;
            if unsafe {
                libc::unlinkat(
                    self.parent.as_raw_fd(),
                    quarantine_c.as_ptr(),
                    libc::AT_REMOVEDIR,
                )
            } != 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
            self.parent.sync_all()?;
            Ok(())
        }

        pub(crate) fn open_directory(&self) -> Result<File> {
            open_at(&self.parent, &self.name, libc::O_RDONLY | libc::O_DIRECTORY)
        }

        pub(crate) fn child(parent: &File, name: OsString) -> Result<Self> {
            Ok(Self {
                parent: parent.try_clone()?,
                name,
            })
        }
    }

    pub(crate) fn directory_names(directory: &File) -> Result<Vec<OsString>> {
        // fdopendir takes ownership. Opening '.' gives enumeration its own offset.
        let file = open_at(
            directory,
            OsStr::new("."),
            libc::O_RDONLY | libc::O_DIRECTORY,
        )?;
        use std::os::fd::IntoRawFd;
        let fd = file.into_raw_fd();
        let stream = unsafe { libc::fdopendir(fd) };
        if stream.is_null() {
            let error = std::io::Error::last_os_error();
            unsafe {
                libc::close(fd);
            }
            return Err(error.into());
        }
        struct Directory(*mut libc::DIR);
        impl Drop for Directory {
            fn drop(&mut self) {
                unsafe {
                    libc::closedir(self.0);
                }
            }
        }
        let stream = Directory(stream);
        let mut names = Vec::new();
        loop {
            // POSIX readdir distinguishes EOF from errors via errno.
            #[cfg(target_os = "linux")]
            unsafe {
                *libc::__errno_location() = 0;
            }
            #[cfg(target_os = "android")]
            unsafe {
                *libc::__errno() = 0;
            }
            #[cfg(target_vendor = "apple")]
            unsafe {
                *libc::__error() = 0;
            }
            let entry = unsafe { libc::readdir(stream.0) };
            if entry.is_null() {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error().unwrap_or(0) != 0 {
                    return Err(error.into());
                }
                break;
            }
            let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if bytes != b"." && bytes != b".." {
                names.push(OsString::from_vec(bytes.to_vec()));
            }
        }
        Ok(names)
    }
}

#[cfg(unix)]
pub(crate) use unix::directory_names;
#[cfg(unix)]
pub use unix::SourcePath;

#[cfg(not(unix))]
pub struct SourcePath;
#[cfg(not(unix))]
impl SourcePath {
    pub fn new(_: &Path) -> Result<Self> {
        Err(StateError::UnsafePath(
            "descriptor-anchored capture is unsupported on this platform".into(),
        ))
    }
    pub fn open_regular(&self) -> Result<File> {
        unreachable!()
    }
    pub fn open_or_create_private(&self) -> Result<File> {
        unreachable!()
    }
    pub fn metadata(&self) -> Result<SourceMetadata> {
        unreachable!()
    }
    pub fn read_link(&self) -> Result<PathBuf> {
        unreachable!()
    }
    pub fn parent_identity(&self) -> Result<(u64, u64)> {
        unreachable!()
    }
    pub fn remove_empty_directory(&self) -> Result<()> {
        unreachable!()
    }
    pub fn delete_leaf(&self, _: &SourceMetadata) -> Result<()> {
        unreachable!()
    }
}

pub fn open_regular_nofollow(path: &Path) -> Result<File> {
    SourcePath::new(path)?.open_regular()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::fs::symlink;

    #[test]
    fn explicit_leaf_deletion_preserves_symlink_targets_and_rejects_replacements() {
        let root = std::env::current_dir()
            .unwrap()
            .join(format!("target/delete-leaf-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        let outside = root.join("outside");
        std::fs::write(&outside, b"keep").unwrap();
        let link = root.join("link");
        symlink(&outside, &link).unwrap();
        let source = SourcePath::new(&link).unwrap();
        source.delete_leaf(&source.metadata().unwrap()).unwrap();
        assert!(!link.exists());
        assert!(outside.exists());
        let source = SourcePath::new(&outside).unwrap();
        let original = source.metadata().unwrap();
        std::fs::rename(&outside, root.join("old")).unwrap();
        std::fs::write(&outside, b"new").unwrap();
        assert!(source.delete_leaf(&original).is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), b"new");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn metadata_does_not_require_file_data_read_permission() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::current_dir()
            .unwrap()
            .join(format!("target/metadata-only-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("unreadable");
        std::fs::write(&path, b"metadata only").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let metadata = SourcePath::new(&path).unwrap().metadata().unwrap();
        assert!(metadata.is_file());
        assert_eq!(metadata.len(), 13);
        assert_eq!(metadata.mode() & 0o777, 0);
        assert!(crate::same_file_identity(
            &metadata,
            &std::fs::symlink_metadata(&path).unwrap()
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn held_parent_cannot_be_redirected_by_ancestor_or_leaf_replacement() {
        let root = std::env::current_dir()
            .unwrap()
            .join(format!("target/anchored-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("owned/nested")).unwrap();
        std::fs::create_dir_all(root.join("outside/nested")).unwrap();
        std::fs::write(root.join("owned/nested/save"), b"owned").unwrap();
        std::fs::write(root.join("outside/nested/save"), b"outside").unwrap();
        let path = root.join("owned/nested/save");
        let anchored = SourcePath::new(&path).unwrap();
        let before = anchored.metadata().unwrap();
        std::fs::rename(root.join("owned"), root.join("detached")).unwrap();
        symlink(root.join("outside"), root.join("owned")).unwrap();
        assert!(crate::same_file_identity(
            &before,
            &anchored.metadata().unwrap()
        ));
        let mut bytes = Vec::new();
        anchored
            .open_regular()
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes, b"owned");
        assert!(open_regular_nofollow(&path).is_err());
        let leaf = root.join("detached/nested/save");
        std::fs::remove_file(&leaf).unwrap();
        symlink(root.join("outside/nested/save"), &leaf).unwrap();
        assert!(anchored.open_regular().is_err());
        assert!(anchored.metadata().unwrap().is_symlink());
        assert!(!crate::same_file_identity(
            &before,
            &anchored.metadata().unwrap()
        ));
        std::fs::remove_file(root.join("outside/nested/save")).unwrap();
        assert!(anchored.metadata().unwrap().is_symlink());
        assert!(crate::same_file_identity(
            &anchored.metadata().unwrap(),
            &std::fs::symlink_metadata(&leaf).unwrap()
        ));
        std::fs::remove_dir_all(root).unwrap();
    }
}
