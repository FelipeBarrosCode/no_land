//! Consistent views: Btrfs snapshot when available, copy fallback otherwise.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use noland_state_core::*;
use uuid::Uuid;

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
        if !source.exists() {
            continue;
        }
        // The source index is injective within this immutable view. Flattening path components can
        // alias distinct paths such as `/a_b/c` and `/a/b_c`, corrupting rollback data.
        let staged = dest.join("copy").join(format!("{source_index:016x}"));
        if let Some(parent) = staged.parent() {
            fs::create_dir_all(parent)?;
        }
        if !copy_stable(source, &staged)? {
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

fn copy_stable(src: &Path, dest: &Path) -> Result<bool> {
    if src.is_dir() {
        copy_dir(src, dest)?;
        return Ok(true);
    }
    let first = fs::metadata(src).ok();
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    copy_file(src, dest)?;
    let second = fs::metadata(src).ok();
    let stable = match (first, second) {
        (Some(ref a), Some(ref b)) => same_file_identity(a, b),
        _ => false,
    };
    if !stable {
        let before_retry = fs::metadata(src).ok();
        copy_file(src, dest)?;
        let third = fs::metadata(src).ok();
        let dest_meta = fs::metadata(dest).ok();
        return Ok(match (before_retry, third, dest_meta) {
            (Some(a), Some(b), Some(c)) => same_file_identity(&a, &b) && b.len() == c.len(),
            _ => false,
        });
    }
    Ok(true)
}

pub fn same_file_identity(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if before.dev() != after.dev()
            || before.ino() != after.ino()
            || before.ctime() != after.ctime()
            || before.ctime_nsec() != after.ctime_nsec()
        {
            return false;
        }
    }
    true
}

fn copy_dir(src: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest)?;
    for entry in read_dir_with_context(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if from.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent)?;
            }
            copy_file(&from, &to)?;
        }
    }
    Ok(())
}

fn copy_file(src: &Path, dest: &Path) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        let source = fs::File::open(src).map_err(|error| {
            StateError::Message(format!("failed to copy {}: {error}", src.display()))
        })?;
        let destination = fs::File::create(dest)?;
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
            destination.set_permissions(source.metadata()?.permissions())?;
            return Ok(());
        }
    }
    noland_state_core::disk::ensure_space(dest, fs::metadata(src)?.len())?;
    fs::copy(src, dest)
        .map(|_| ())
        .map_err(|error| StateError::Message(format!("failed to copy {}: {error}", src.display())))
}

fn read_dir_with_context(src: &Path) -> Result<fs::ReadDir> {
    fs::read_dir(src).map_err(|error| {
        StateError::Message(format!(
            "failed to read directory {}: {error}",
            src.display()
        ))
    })
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
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let status = Command::new("btrfs")
        .args(["subvolume", "snapshot", "-r"])
        .arg(src)
        .arg(dest)
        .status();
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(StateError::msg(format!("btrfs snapshot failed: {s}"))),
        Err(err) => Err(err.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn failed_copy_removes_only_its_partial_snapshot_and_preserves_sources() {
        let tmp = std::env::temp_dir().join(format!("noland-snap-failure-{}", Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        let save = tmp.join("save.dat");
        fs::write(&save, b"preserve this save").unwrap();
        // macOS limits Unix socket paths to roughly 100 bytes; the approved
        // temporary directory alone may already occupy most of that budget.
        let socket =
            std::env::temp_dir().join(format!("s-{}", &Uuid::new_v4().simple().to_string()[..12]));
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
        let tmp = std::env::temp_dir().join(format!("noland-snap-alias-{}", Uuid::new_v4()));
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
        let tmp = std::env::temp_dir().join(format!("noland-snap-{}", Uuid::new_v4()));
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
