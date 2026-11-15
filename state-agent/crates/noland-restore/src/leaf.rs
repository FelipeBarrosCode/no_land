//! Anchored, leaf-only tombstone mutation. Directory handles are short lived;
//! the plan retains identities, never one descriptor per manifest entry.
use std::ffi::CString;
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path};

use noland_state_core::{Result, StateError};

use crate::{identity::VerifiedIdentity, PlannedTombstone};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct Observation {
    parent: Option<(u64, u64)>,
    pub identity: Option<VerifiedIdentity>,
}

impl Observation {
    pub fn same_parent(self, current: Self) -> bool {
        self.parent == current.parent || (self.parent.is_none() && self.identity.is_none())
    }

    fn unchanged(self, current: Self) -> bool {
        self.same_parent(current) && self.identity == current.identity
    }
}

struct Parent {
    directory: File,
    name: CString,
}

fn cstring(name: &std::ffi::OsStr) -> Result<CString> {
    CString::new(name.as_bytes()).map_err(|_| StateError::UnsafePath("NUL in restore path".into()))
}

impl Parent {
    fn open(destination: &Path) -> Result<Option<Self>> {
        crate::validate_absolute_journal_path(destination, "tombstone")?;
        let parent = destination
            .parent()
            .ok_or_else(|| StateError::UnsafePath("tombstone has no parent".into()))?;
        let name = cstring(
            destination
                .file_name()
                .ok_or_else(|| StateError::UnsafePath("tombstone has no filename".into()))?,
        )?;
        let mut directory = File::open("/")?;
        for component in parent.components() {
            let Component::Normal(component) = component else {
                continue;
            };
            let name = cstring(component)?;
            // Each lookup is relative to a pinned, no-follow ancestor. A parent
            // swap cannot redirect the unlink to a different directory tree.
            let fd = unsafe {
                libc::openat(
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::NotFound {
                    return Ok(None);
                }
                return Err(error.into());
            }
            directory = unsafe { File::from_raw_fd(fd) };
        }
        Ok(Some(Self { directory, name }))
    }

    fn observe(&self) -> Result<Observation> {
        let parent = self.directory.metadata()?;
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        let result = unsafe {
            libc::fstatat(
                self.directory.as_raw_fd(),
                self.name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        let identity = if result == 0 {
            Some(VerifiedIdentity::from_stat(&unsafe { stat.assume_init() }))
        } else {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(error.into());
            }
            None
        };
        Ok(Observation {
            parent: Some((parent.dev(), parent.ino())),
            identity,
        })
    }
}

pub(super) fn observe(destination: &Path) -> Result<Observation> {
    Parent::open(destination)?.map_or(
        Ok(Observation {
            parent: None,
            identity: None,
        }),
        |parent| parent.observe(),
    )
}

impl PlannedTombstone {
    pub(super) fn validate(&self) -> Result<()> {
        crate::validate_parent_chain(&self.root, &self.destination)?;
        if !self.observation.unchanged(observe(&self.destination)?) {
            return Err(StateError::Integrity(
                "tombstone destination changed since preflight".into(),
            ));
        }
        Ok(())
    }

    pub(super) fn apply(&self) -> Result<()> {
        crate::validate_parent_chain(&self.root, &self.destination)?;
        let parent = Parent::open(&self.destination)?;
        let current = match &parent {
            Some(parent) => parent.observe()?,
            None => Observation {
                parent: None,
                identity: None,
            },
        };
        if !self.observation.unchanged(current)
            || current.identity.is_some_and(VerifiedIdentity::is_directory)
        {
            return Err(StateError::Integrity(
                "tombstone destination changed since preflight".into(),
            ));
        }
        if current.identity.is_none() {
            return Ok(());
        }
        let parent = parent.expect("observed leaf has a parent");
        // No AT_REMOVEDIR: even a directory replacement after the last stat is
        // rejected by the kernel rather than recursively deleting its contents.
        if unsafe { libc::unlinkat(parent.directory.as_raw_fd(), parent.name.as_ptr(), 0) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        parent.directory.sync_all()?;
        Ok(())
    }
}
