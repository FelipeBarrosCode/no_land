use std::path::{Path, PathBuf};

use chrono::Utc;
use noland_state_core::*;
use walkdir_lite::Walk;

use crate::StateAgent;

pub fn reconcile_app(agent: &StateAgent, app_id: &AppId) -> Result<usize> {
    noland_state_core::metrics::Metrics::inc(&agent.metrics.reconciliations_total);
    let dirty_roots = agent.db.list_dirty_roots(Some(app_id))?;
    let mut roots: Vec<PathBuf> = dirty_roots
        .iter()
        .map(|root| PathBuf::from(&root.canonical_root))
        .collect();
    if roots.is_empty() {
        roots = agent
            .db
            .known_roots(Some(app_id))?
            .into_iter()
            .map(|(_, _, p)| PathBuf::from(p))
            .collect();
        for (record, association) in agent.db.associations_for_app(app_id)? {
            if let Some(root) = reconciliation_root(&record, &association) {
                roots.push(root);
            }
        }
    }
    roots.sort();
    roots.dedup();
    let mut found = 0;
    let now = Utc::now();
    for root in roots {
        if !root.exists() || is_hard_volatile_root(&root) || agent.config.paths.is_internal(&root) {
            continue;
        }
        for path in Walk::new(&root).max_depth(6) {
            if looks_like_cache(&path) || looks_like_lock_or_socket(&path) {
                continue;
            }
            let canonical = path.to_string_lossy().into_owned();
            let path_id = agent.db.upsert_path(&canonical)?;
            let existing = agent.db.associations_for_path(path_id)?;
            let logical = agent.roots.lock().classify(&path);
            let metadata = std::fs::metadata(&path).ok();
            #[cfg(unix)]
            use std::os::unix::fs::MetadataExt;
            let record = PathRecord {
                path_id,
                canonical_path: canonical,
                logical_root: logical.as_ref().map(|l| l.logical_root.as_token()),
                relative_path: logical.as_ref().map(|l| l.relative_path.clone()),
                file_type: Some(if path.is_dir() { "directory" } else { "file" }.into()),
                #[cfg(unix)]
                inode: metadata.as_ref().map(|value| value.ino() as i64),
                #[cfg(not(unix))]
                inode: None,
                mount_id: None,
                size: metadata.as_ref().map(|value| value.len() as i64),
                mtime_ns: metadata.as_ref().and_then(metadata_mtime_ns),
                #[cfg(unix)]
                mode: metadata.as_ref().map(|value| value.mode() as i64),
                #[cfg(not(unix))]
                mode: None,
                #[cfg(unix)]
                uid: metadata.as_ref().map(|value| value.uid() as i64),
                #[cfg(not(unix))]
                uid: None,
                #[cfg(unix)]
                gid: metadata.as_ref().map(|value| value.gid() as i64),
                #[cfg(not(unix))]
                gid: None,
                content_hash: None,
                last_scanned_at: Some(now.timestamp()),
            };
            agent.db.update_path_meta(path_id, &record)?;
            if let Some(logical) = logical.as_ref() {
                agent.db.upsert_file_state(&FileStateRecord {
                    app_id: app_id.clone(),
                    logical_root: logical.logical_root.as_token(),
                    relative_path: logical.relative_path.clone(),
                    canonical_path: Some(record.canonical_path.clone()),
                    file_type: if path.is_dir() {
                        FileType::Directory
                    } else {
                        FileType::File
                    },
                    size: metadata.as_ref().map(|value| value.len()).unwrap_or(0),
                    mtime_ns: record.mtime_ns.unwrap_or(0),
                    inode: record.inode.and_then(|value| u64::try_from(value).ok()),
                    mount_id: record.mount_id.and_then(|value| u64::try_from(value).ok()),
                    mode: record.mode.and_then(|value| u32::try_from(value).ok()),
                    content_hash: record.content_hash.clone(),
                    trust: FileStateTrust::VerifyRequired,
                    last_seen_at: now,
                    last_hashed_at: None,
                })?;
            }
            if existing
                .iter()
                .any(|association| association.app_id == *app_id)
            {
                found += 1;
                continue;
            }
            agent.db.upsert_association(&PathAssociation {
                app_id: app_id.clone(),
                path_id,
                confidence: CONF_REPEATED,
                evidence: vec![Evidence::new(EvidenceKind::ReconciliationDelta)],
                persistence_class: if looks_like_user_state(&path) {
                    PersistenceClass::PersistentState
                } else {
                    PersistenceClass::Unknown
                },
                semantic_role: infer_semantic_role(
                    &path,
                    if looks_like_user_state(&path) {
                        PersistenceClass::PersistentState
                    } else {
                        PersistenceClass::Unknown
                    },
                ),
                first_seen_at: now,
                last_seen_at: now,
            })?;
            found += 1;
        }
    }
    agent.db.clear_reconciliation_required(app_id)?;
    agent.db.clear_dirty_roots(app_id)?;
    Ok(found)
}

fn metadata_mtime_ns(metadata: &std::fs::Metadata) -> Option<i64> {
    metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
}

fn reconciliation_root(record: &PathRecord, association: &PathAssociation) -> Option<PathBuf> {
    let path = Path::new(&record.canonical_path);
    if association.persistence_class == PersistenceClass::Ephemeral
        || association.persistence_class == PersistenceClass::BaseImage
        || !looks_like_user_state(path)
    {
        return None;
    }
    path.parent().map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn association(class: PersistenceClass) -> PathAssociation {
        PathAssociation {
            app_id: AppId("desktop:test".into()),
            path_id: 1,
            confidence: 0.9,
            evidence: Vec::new(),
            persistence_class: class,
            semantic_role: SemanticRole::UserState,
            first_seen_at: Utc::now(),
            last_seen_at: Utc::now(),
        }
    }

    fn record(path: &str) -> PathRecord {
        PathRecord {
            path_id: 1,
            canonical_path: path.into(),
            logical_root: None,
            relative_path: None,
            file_type: Some("file".into()),
            inode: None,
            mount_id: None,
            size: None,
            mtime_ns: None,
            mode: None,
            uid: None,
            gid: None,
            content_hash: None,
            last_scanned_at: None,
        }
    }

    #[test]
    fn reconciliation_scans_user_state_but_not_media_or_system_dependencies() {
        assert_eq!(
            reconciliation_root(
                &record("/home/user/.config/PCSX2/memcards/Mcd001.ps2"),
                &association(PersistenceClass::PersistentState),
            ),
            Some(PathBuf::from("/home/user/.config/PCSX2/memcards"))
        );
        assert_eq!(
            reconciliation_root(
                &record("/home/user/Downloads/Grand Theft Auto.iso"),
                &association(PersistenceClass::Unknown),
            ),
            None
        );
        assert_eq!(
            reconciliation_root(
                &record("/usr/lib/x86_64-linux-gnu/libc.so.6"),
                &association(PersistenceClass::BaseImage),
            ),
            None
        );
    }
}

/// Tiny walk helper so we do not take a walkdir crate dependency.
mod walkdir_lite {
    use std::path::{Path, PathBuf};

    pub struct Walk {
        stack: Vec<(PathBuf, usize)>,
        max_depth: usize,
    }

    impl Walk {
        pub fn new(root: &Path) -> Self {
            Self {
                stack: vec![(root.to_path_buf(), 0)],
                max_depth: 8,
            }
        }

        pub fn max_depth(mut self, depth: usize) -> Self {
            self.max_depth = depth;
            self
        }
    }

    impl Iterator for Walk {
        type Item = PathBuf;

        fn next(&mut self) -> Option<Self::Item> {
            let (path, depth) = self.stack.pop()?;
            if path.is_dir() && depth < self.max_depth {
                if let Ok(entries) = std::fs::read_dir(&path) {
                    for entry in entries.flatten() {
                        self.stack.push((entry.path(), depth + 1));
                    }
                }
            }
            Some(path)
        }
    }
}
