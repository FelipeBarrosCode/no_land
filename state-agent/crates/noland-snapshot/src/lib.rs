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

pub fn create_view(
    snapshot_root: &Path,
    sources: &[PathBuf],
    prefer_btrfs: bool,
) -> Result<SnapshotView> {
    let id = Uuid::new_v4();
    let dest = snapshot_root.join(id.to_string());
    fs::create_dir_all(&dest)?;

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
    for source in sources {
        if !source.exists() {
            continue;
        }
        let name = unique_name(source);
        let staged = dest.join("copy").join(&name);
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
    if view.root.exists() {
        fs::remove_dir_all(&view.root)?;
    }
    Ok(())
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
    fs::copy(src, dest)?;
    let second = fs::metadata(src).ok();
    let stable = match (first, second) {
        (Some(a), Some(b)) => a.len() == b.len() && a.modified().ok() == b.modified().ok(),
        _ => false,
    };
    if !stable {
        fs::copy(src, dest)?;
        let third = fs::metadata(src).ok();
        let dest_meta = fs::metadata(dest).ok();
        return Ok(match (third, dest_meta) {
            (Some(a), Some(b)) => a.len() == b.len(),
            _ => false,
        });
    }
    Ok(true)
}

fn copy_dir(src: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if from.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

fn unique_name(path: &Path) -> String {
    path.iter()
        .map(|c| c.to_string_lossy())
        .collect::<Vec<_>>()
        .join("_")
        .trim_start_matches('_')
        .replace('/', "_")
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
