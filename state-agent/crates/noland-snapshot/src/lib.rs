//! Consistent views: Btrfs snapshot when available, copy fallback otherwise.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use noland_state_core::*;
use uuid::Uuid;

mod metadata;
mod source;
pub use metadata::{same_file_identity, SourceMetadata};
pub use source::{open_regular_nofollow, SourcePath};

#[derive(Debug, Clone)]
pub struct SnapshotView {
    pub id: Uuid,
    pub root: PathBuf,
    pub consistency: ConsistencyKind,
    pub mappings: Vec<PathMapping>,
}

#[derive(Debug, Clone)]
pub struct PathMapping {
    pub source: PathBuf,
    pub staged: PathBuf,
}

/// A failed copy must not leave a partial snapshot behind. Successful views are
/// still owned by the caller and retained for backup or restore rollback.
struct IncompleteView {
    root: PathBuf,
    complete: bool,
}

impl Drop for IncompleteView {
    fn drop(&mut self) {
        if !self.complete {
            let _ = discard_root(&self.root);
        }
    }
}

pub fn create_view(
    snapshot_root: &Path,
    sources: &[PathBuf],
    prefer_btrfs: bool,
) -> Result<SnapshotView> {
    create_view_observed(snapshot_root, sources, prefer_btrfs, |_| Ok(()))
}

pub fn create_view_observed(
    snapshot_root: &Path,
    sources: &[PathBuf],
    prefer_btrfs: bool,
    mut on_file: impl FnMut(u64) -> Result<()>,
) -> Result<SnapshotView> {
    let id = Uuid::new_v4();
    let dest = snapshot_root.join(id.to_string());
    fs::create_dir_all(&dest)?;
    let mut cleanup = IncompleteView {
        root: dest.clone(),
        complete: false,
    };

    if prefer_btrfs {
        if let Some(common) = common_btrfs_parent(sources) {
            if try_btrfs_snapshot(&common, &dest.join("btrfs")).is_ok() {
                let mappings = sources
                    .iter()
                    .filter_map(|src| {
                        let rel = src.strip_prefix(&common).ok()?;
                        Some(PathMapping {
                            source: src.clone(),
                            staged: dest.join("btrfs").join(rel),
                        })
                    })
                    .collect();
                cleanup.complete = true;
                return Ok(SnapshotView {
                    id,
                    root: dest,
                    consistency: ConsistencyKind::Snapshot,
                    mappings,
                });
            }
        }
    }

    let mut mappings = Vec::new();
    let mut consistent = true;
    for (source_index, source) in sources.iter().enumerate() {
        on_file(source_index as u64)?;
        // The source index is injective within this immutable view. Flattening path components can
        // alias distinct paths such as `/a_b/c` and `/a/b_c`, corrupting rollback data.
        let staged = dest.join("copy").join(format!("{source_index:016x}"));
        if let Some(parent) = staged.parent() {
            fs::create_dir_all(parent)?;
        }
        let anchored = match SourcePath::new(source) {
            Ok(anchored) => anchored,
            Err(StateError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let metadata = match anchored.metadata() {
            Ok(metadata) => metadata,
            Err(StateError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(StateError::Message(format!(
                    "failed to inspect {}: {error}",
                    source.display()
                )))
            }
        };
        if !copy_entry(&anchored, &metadata, &staged).map_err(|error| {
            StateError::Message(format!("failed to copy {}: {error}", source.display()))
        })? {
            consistent = false;
        }
        mappings.push(PathMapping {
            source: source.clone(),
            staged,
        });
    }
    on_file(sources.len() as u64)?;
    cleanup.complete = true;
    Ok(SnapshotView {
        id,
        root: dest,
        consistency: if consistent {
            ConsistencyKind::Snapshot
        } else {
            ConsistencyKind::BestEffort
        },
        mappings,
    })
}

pub fn discard(view: &SnapshotView) -> Result<()> {
    discard_root(&view.root)
}

pub fn discard_root(root: &Path) -> Result<()> {
    let subvolume = root.join("btrfs");
    if fs::symlink_metadata(&subvolume).is_ok_and(|meta| meta.is_dir()) {
        let status = Command::new("btrfs")
            .args(["subvolume", "delete"])
            .arg(&subvolume)
            .output()?;
        if !status.status.success() {
            return Err(StateError::Storage(
                "failed to remove owned snapshot subvolume".into(),
            ));
        }
    }
    if root.exists() {
        fs::remove_dir_all(root)?;
    }
    Ok(())
}

/// Attempt one atomic Btrfs view for the whole capture. Callers can then use
/// bounded per-file CoW/copy staging when the filesystem has no snapshot support.
pub fn create_atomic_view(
    snapshot_root: &Path,
    sources: &[PathBuf],
) -> Result<Option<SnapshotView>> {
    let Some(common) = common_btrfs_parent(sources) else {
        return Ok(None);
    };
    let id = Uuid::new_v4();
    let root = snapshot_root.join(id.to_string());
    fs::create_dir_all(&root)?;
    let mut cleanup = IncompleteView {
        root: root.clone(),
        complete: false,
    };
    if try_btrfs_snapshot(&common, &root.join("btrfs")).is_err() {
        return Ok(None);
    }
    let mappings = sources
        .iter()
        .map(|source| {
            Ok(PathMapping {
                source: source.clone(),
                staged: root.join("btrfs").join(
                    source.strip_prefix(&common).map_err(|_| {
                        StateError::UnsafePath("snapshot source outside root".into())
                    })?,
                ),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    cleanup.complete = true;
    Ok(Some(SnapshotView {
        id,
        root,
        consistency: ConsistencyKind::Snapshot,
        mappings,
    }))
}

/// Attempt descriptor-based CoW without ever falling back to a whole-file copy.
/// Streaming callers copy/hash into durable segments themselves when unavailable.
pub fn create_cow_file_view(
    snapshot_root: &Path,
    source: &Path,
    file: &fs::File,
) -> Result<Option<SnapshotView>> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        let before = file.metadata()?;
        let id = Uuid::new_v4();
        let root = snapshot_root.join(id.to_string());
        fs::create_dir_all(&root)?;
        let mut cleanup = IncompleteView {
            root: root.clone(),
            complete: false,
        };
        let staged = root.join("capture");
        let destination = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged)?;
        if unsafe {
            libc::ioctl(
                destination.as_raw_fd(),
                0x40049409u64 as libc::c_ulong,
                file.as_raw_fd(),
            )
        } != 0
        {
            return Ok(None);
        }
        if !same_file_identity(&before, &file.metadata()?)
            || destination.metadata()?.len() != before.len()
        {
            return Err(StateError::Conflict(
                "source changed during CoW capture".into(),
            ));
        }
        let mut permissions = destination.metadata()?.permissions();
        permissions.set_readonly(true);
        destination.set_permissions(permissions)?;
        cleanup.complete = true;
        Ok(Some(SnapshotView {
            id,
            root,
            consistency: ConsistencyKind::Snapshot,
            mappings: vec![PathMapping {
                source: source.to_path_buf(),
                staged,
            }],
        }))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (snapshot_root, source, file);
        Ok(None)
    }
}

/// Stage an already opened source. Neither retries nor CoW fallback reopen the
/// live pathname, so its leaf and ancestors may be replaced without redirection.
pub fn create_file_view(
    snapshot_root: &Path,
    source: &Path,
    file: &mut fs::File,
) -> Result<(SnapshotView, fs::Metadata)> {
    let id = Uuid::new_v4();
    let root = snapshot_root.join(id.to_string());
    fs::create_dir_all(&root)?;
    let mut cleanup = IncompleteView {
        root: root.clone(),
        complete: false,
    };
    let staged = root.join("capture");
    let (stable, metadata) = copy_stable_file(file, &staged)?;
    cleanup.complete = true;
    Ok((
        SnapshotView {
            id,
            root,
            consistency: if stable {
                ConsistencyKind::Snapshot
            } else {
                ConsistencyKind::BestEffort
            },
            mappings: vec![PathMapping {
                source: source.to_path_buf(),
                staged,
            }],
        },
        metadata,
    ))
}

fn copy_stable_file(file: &mut fs::File, dest: &Path) -> Result<(bool, fs::Metadata)> {
    use std::io::{Seek, SeekFrom};
    let mut destination = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dest)?;
    for attempt in 0..2 {
        let before = file.metadata()?;
        if !before.is_file() {
            return Err(StateError::UnsafePath(
                "snapshot source is not a regular file".into(),
            ));
        }
        file.seek(SeekFrom::Start(0))?;
        destination.set_len(0)?;
        destination.seek(SeekFrom::Start(0))?;
        copy_file(file, &mut destination, dest, &before)?;
        let after = file.metadata()?;
        let stable =
            same_file_identity(&before, &after) && destination.metadata()?.len() == before.len();
        if stable || attempt == 1 {
            return Ok((stable, before));
        }
    }
    unreachable!()
}

fn copy_entry(src: &SourcePath, before: &SourceMetadata, dest: &Path) -> Result<bool> {
    #[cfg(unix)]
    if before.is_symlink() {
        let target = src.read_link()?;
        if !same_file_identity(before, &src.metadata()?) {
            return Err(StateError::Integrity(
                "snapshot symlink changed during capture".into(),
            ));
        }
        std::os::unix::fs::symlink(target, dest)?;
        return Ok(true);
    } else if before.is_dir() {
        let directory = src.open_directory()?;
        if !same_file_identity(before, &directory.metadata()?) {
            return Err(StateError::Integrity(
                "snapshot directory changed before capture".into(),
            ));
        }
        fs::create_dir_all(dest)?;
        let mut stable = true;
        for name in source::directory_names(&directory)? {
            let child = SourcePath::child(&directory, name.clone())?;
            stable &= copy_entry(&child, &child.metadata()?, &dest.join(name))?;
        }
        return Ok(stable && same_file_identity(before, &directory.metadata()?));
    }
    if !before.is_file() {
        return Err(StateError::UnsafePath(
            "unsupported snapshot source type".into(),
        ));
    }
    let mut file = src.open_regular()?;
    if !same_file_identity(before, &file.metadata()?) {
        return Err(StateError::Integrity(
            "snapshot source changed before open".into(),
        ));
    }
    let (stable, _) = copy_stable_file(&mut file, dest)?;
    Ok(stable)
}

fn copy_file(
    source: &mut fs::File,
    destination: &mut fs::File,
    dest: &Path,
    before: &fs::Metadata,
) -> Result<()> {
    use std::io::{Read, Write};
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        // FICLONE creates independent CoW extents, never a mutable hardlink.
        // Unsupported/cross-device filesystems use the normal stable-copy fallback.
        if unsafe {
            libc::ioctl(
                destination.as_raw_fd(),
                0x40049409u64 as libc::c_ulong,
                source.as_raw_fd(),
            )
        } == 0
        {
            destination.set_permissions(before.permissions())?;
            return Ok(());
        }
        destination.set_len(0)?;
    }
    let mut budget = disk::DiskWriteBudget::reserve(dest, before.len())?;
    let mut remaining = before.len();
    let mut buffer = [0u8; 64 * 1024];
    while remaining != 0 {
        let limit = remaining.min(buffer.len() as u64) as usize;
        let read = source.read(&mut buffer[..limit])?;
        if read == 0 {
            break;
        }
        destination.write_all(&buffer[..read])?;
        budget.consume_allocated(read as u64)?;
        remaining -= read as u64;
    }
    destination.set_permissions(before.permissions())?;
    Ok(())
}

fn common_btrfs_parent(sources: &[PathBuf]) -> Option<PathBuf> {
    let first = sources.first()?;
    let mut parent = first.parent()?.to_path_buf();
    for src in sources.iter().skip(1) {
        while !src.starts_with(&parent) {
            parent = parent.parent()?.to_path_buf();
        }
    }
    Some(parent)
}

fn try_btrfs_snapshot(src: &Path, dest: &Path) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};
        let source = SourcePath::new(src)?.open_directory()?;
        let parent = dest
            .parent()
            .ok_or_else(|| StateError::UnsafePath("snapshot has no parent".into()))?;
        let destination = SourcePath::new(parent)?.open_directory()?;
        let name = dest
            .file_name()
            .ok_or_else(|| StateError::UnsafePath("snapshot has no name".into()))?
            .as_bytes();
        #[repr(C)]
        struct Args {
            fd: i64,
            transid: u64,
            flags: u64,
            unused: [u64; 4],
            name: [u8; 4040],
        }
        let mut args = Args {
            fd: source.as_raw_fd() as i64,
            transid: 0,
            flags: 2,
            unused: [0; 4],
            name: [0; 4040],
        };
        if name.len() >= args.name.len() || name.contains(&0) {
            return Err(StateError::UnsafePath("invalid snapshot name".into()));
        }
        args.name[..name.len()].copy_from_slice(name);
        // BTRFS_IOC_SNAP_CREATE_V2, BTRFS_SUBVOL_RDONLY. Both source and target
        // parent are held descriptors; invoking btrfs with live paths is racy.
        if unsafe {
            libc::ioctl(
                destination.as_raw_fd(),
                0x50009417u64 as libc::c_ulong,
                &mut args,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (src, dest);
        Err(StateError::Storage("Btrfs snapshot unavailable".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn descriptor_copy_ignores_replaced_names_and_directory_capture_preserves_links() {
        use std::os::unix::fs::symlink;
        let root = std::env::current_dir()
            .unwrap()
            .join(format!("target/snapshot-race-{}", Uuid::new_v4()));
        fs::create_dir_all(root.join("source/nested")).unwrap();
        fs::create_dir_all(root.join("outside/nested")).unwrap();
        let source = root.join("source/nested/file");
        let outside = root.join("outside/nested/file");
        fs::write(&source, b"owned snapshot").unwrap();
        fs::write(&outside, b"unowned target").unwrap();
        let mut held = open_regular_nofollow(&source).unwrap();
        fs::rename(root.join("source"), root.join("detached")).unwrap();
        symlink(root.join("outside"), root.join("source")).unwrap();
        fs::remove_file(root.join("detached/nested/file")).unwrap();
        symlink(&outside, root.join("detached/nested/file")).unwrap();
        let (view, _) = create_file_view(&root.join("snapshots"), &source, &mut held).unwrap();
        assert_eq!(
            fs::read(&view.mappings[0].staged).unwrap(),
            b"owned snapshot"
        );
        assert_eq!(view.consistency, ConsistencyKind::Snapshot);
        discard(&view).unwrap();
        assert!(create_view(&root.join("snapshots"), &[source], false).is_err());
        let tree = create_view(&root.join("snapshots"), &[root.join("detached")], false).unwrap();
        let link = tree.mappings[0].staged.join("nested/file");
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read_link(link).unwrap(), outside);
        discard(&tree).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn failed_copy_removes_only_its_partial_snapshot_and_preserves_sources() {
        let tmp = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("noland-snap-failure-{}", Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        let save = tmp.join("save.dat");
        fs::write(&save, b"preserve this save").unwrap();
        // macOS limits Unix socket paths to roughly 100 bytes; the approved
        // temporary directory alone may already occupy most of that budget.
        let socket = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("s-{}", &Uuid::new_v4().simple().to_string()[..12]));
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let snapshots = tmp.join("snapshots");
        fs::create_dir_all(snapshots.join("previous-successful-view")).unwrap();
        fs::write(snapshots.join("previous-successful-view/keep"), b"previous").unwrap();
        let error = create_view(&snapshots, &[save.clone(), socket.clone()], false).unwrap_err();
        assert!(error
            .to_string()
            .contains(&socket.to_string_lossy().to_string()));
        assert_eq!(fs::read(&save).unwrap(), b"preserve this save");
        assert_eq!(fs::read_dir(&snapshots).unwrap().count(), 1);
        assert_eq!(
            fs::read(snapshots.join("previous-successful-view/keep")).unwrap(),
            b"previous"
        );
        fs::remove_file(socket).unwrap();
        fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn copy_fallback_uses_distinct_artifacts_for_ambiguous_paths() {
        let tmp = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("noland-snap-alias-{}", Uuid::new_v4()));
        let first = tmp.join("a_b/c");
        let second = tmp.join("a/b_c");
        fs::create_dir_all(first.parent().unwrap()).unwrap();
        fs::create_dir_all(second.parent().unwrap()).unwrap();
        fs::write(&first, b"first").unwrap();
        fs::write(&second, b"second").unwrap();

        let view =
            create_view(&tmp.join("snaps"), &[first.clone(), second.clone()], false).unwrap();

        assert_ne!(view.mappings[0].staged, view.mappings[1].staged);
        assert_eq!(fs::read(&view.mappings[0].staged).unwrap(), b"first");
        assert_eq!(fs::read(&view.mappings[1].staged).unwrap(), b"second");
        discard(&view).unwrap();
        fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn copy_fallback_preserves_bytes() {
        let tmp = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("noland-snap-{}", Uuid::new_v4()));
        let src_dir = tmp.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        let file = src_dir.join("save.dat");
        fs::write(&file, b"world-v1").unwrap();
        let view = create_view(&tmp.join("snaps"), &[file.clone()], false).unwrap();
        let staged = &view.mappings[0].staged;
        assert_eq!(fs::read(staged).unwrap(), b"world-v1");
        discard(&view).unwrap();
        fs::remove_dir_all(tmp).ok();
    }
}
