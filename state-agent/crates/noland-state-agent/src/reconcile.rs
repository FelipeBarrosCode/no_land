use std::path::{Path, PathBuf};

use chrono::Utc;
use noland_state_core::*;
use walkdir_lite::Walk;

use crate::StateAgent;

pub fn reconcile_app(agent: &StateAgent, app_id: &AppId) -> Result<usize> {
    noland_state_core::metrics::Metrics::inc(&agent.metrics.reconciliations_total);
    let mut roots: Vec<PathBuf> = agent
        .db
        .known_roots(Some(app_id))?
        .into_iter()
        .map(|(_, _, p)| PathBuf::from(p))
        .collect();
    for (record, _) in agent.db.associations_for_app(app_id)? {
        if let Some(parent) = Path::new(&record.canonical_path).parent() {
            roots.push(parent.to_path_buf());
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
            if existing.iter().any(|a| a.app_id == *app_id) {
                continue;
            }
            let logical = agent.roots.lock().classify(&path);
            let record = PathRecord {
                path_id,
                canonical_path: canonical,
                logical_root: logical.as_ref().map(|l| l.logical_root.as_token()),
                relative_path: logical.as_ref().map(|l| l.relative_path.clone()),
                file_type: Some(if path.is_dir() { "directory" } else { "file" }.into()),
                inode: None,
                mount_id: None,
                size: std::fs::metadata(&path).ok().map(|m| m.len() as i64),
                mtime_ns: None,
                mode: None,
                uid: None,
                gid: None,
                content_hash: None,
                last_scanned_at: Some(now.timestamp()),
            };
            agent.db.update_path_meta(path_id, &record)?;
            let _ = record;
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
    agent.db.mark_dirty(app_id, None, false)?;
    Ok(found)
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
