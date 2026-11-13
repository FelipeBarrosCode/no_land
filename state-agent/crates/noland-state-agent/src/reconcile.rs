use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::Utc;
use noland_discovery::{classify_observed_path, PathDisposition};
use noland_state_core::*;
use walkdir_lite::Walk;

use crate::StateAgent;

#[derive(Debug, Clone)]
pub(crate) struct SteamBackupScope {
    install_roots: Vec<PathBuf>,
    explicit_roots: Vec<PathBuf>,
    application_roots: Vec<PathBuf>,
    metadata_files: Vec<PathBuf>,
}

impl SteamBackupScope {
    pub(crate) fn load(agent: &StateAgent, app_id: &AppId) -> Result<Option<Self>> {
        if !app_id.as_str().starts_with("steam:") {
            return Ok(None);
        }
        let associations = agent.db.associations_for_app(app_id)?;
        Self::load_with_associations(agent, app_id, &associations)
    }

    fn load_with_associations(
        agent: &StateAgent,
        app_id: &AppId,
        associations: &[(PathRecord, PathAssociation)],
    ) -> Result<Option<Self>> {
        let Some(steam_app_id) = app_id
            .as_str()
            .strip_prefix("steam:")
            .and_then(|value| value.parse::<u32>().ok())
        else {
            return Ok(None);
        };

        let install_roots: Vec<PathBuf> = known_install_roots(agent, app_id)?
            .into_iter()
            .filter(|root| steam_install_root_allowed(root))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let explicit_roots: Vec<PathBuf> = associations
            .iter()
            .filter(|(_, association)| {
                association
                    .evidence
                    .iter()
                    .any(|evidence| evidence.kind == EvidenceKind::ExplicitUserBinding)
            })
            .map(|(record, _)| PathBuf::from(&record.canonical_path))
            .filter(|root| explicit_scope_path_allowed(root))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        let metadata_files: Vec<PathBuf> =
            steam_appmanifest_path(steam_app_id, &agent.roots.lock(), &install_roots)
                .into_iter()
                .collect();

        // These namespaces are owned by this exact Steam app. Never scan the
        // shared Steam download/runtime trees or another game's prefix/cache.
        let roots = agent.roots.lock().clone();
        let mut application_roots = agent
            .db
            .known_roots(Some(app_id))?
            .into_iter()
            .filter(|(_, kind, _)| {
                matches!(
                    kind.as_str(),
                    "state" | "user-state" | "save" | "config" | "cache"
                )
            })
            .map(|(_, _, path)| PathBuf::from(path))
            .collect::<Vec<_>>();
        for library in roots.steam_libraries.values() {
            application_roots.push(library.join("shadercache").join(steam_app_id.to_string()));
            application_roots.push(library.join("compatdata").join(steam_app_id.to_string()));
        }
        if let Some(prefix) = roots.proton_prefixes.get(&steam_app_id) {
            application_roots.push(prefix.clone());
        }
        let userdata = roots
            .steam_root
            .as_ref()
            .map(|root| root.join("userdata"))
            .or(roots.steam_userdata);
        if let Some(userdata) = userdata {
            match std::fs::read_dir(userdata) {
                Ok(users) => {
                    for user in users {
                        let user = user?;
                        if user.file_type()?.is_dir()
                            && user.file_name().to_str().is_some_and(|name| {
                                !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit())
                            })
                        {
                            application_roots.push(user.path().join(steam_app_id.to_string()));
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        application_roots.retain(|path| explicit_scope_path_allowed(path));
        application_roots.sort();
        application_roots.dedup();

        Ok(Some(Self {
            install_roots,
            explicit_roots,
            application_roots,
            metadata_files,
        }))
    }

    pub(crate) fn install_roots(&self) -> &[PathBuf] {
        &self.install_roots
    }

    pub(crate) fn reconciliation_roots(&self) -> Vec<PathBuf> {
        self.install_roots
            .iter()
            .chain(&self.explicit_roots)
            .chain(&self.application_roots)
            .cloned()
            .collect()
    }

    pub(crate) fn contains(&self, path: &Path) -> bool {
        is_valid_index_path(path)
            && (self.metadata_files.iter().any(|metadata| path == metadata)
                || (steam_scope_path_allowed(path)
                    && self.install_roots.iter().any(|root| path.starts_with(root)))
                || self.contains_explicit(path)
                || (explicit_scope_path_allowed(path)
                    && self
                        .application_roots
                        .iter()
                        .any(|root| path.starts_with(root))))
    }

    fn contains_explicit(&self, path: &Path) -> bool {
        explicit_scope_path_allowed(path)
            && path.ancestors().any(|ancestor| {
                self.explicit_roots
                    .binary_search_by(|root| root.as_path().cmp(ancestor))
                    .is_ok()
            })
    }

    fn can_traverse(&self, path: &Path) -> bool {
        // A filename-only exclusion must not hide children of a directory with
        // that name. Tree exclusions (cache, staging, runtime) still prune.
        self.contains(path) || self.contains(&path.join("_scan_descendant"))
    }
}

pub(crate) fn steam_appmanifest_path(
    steam_app_id: u32,
    roots: &LogicalRootMap,
    install_roots: &[PathBuf],
) -> Option<PathBuf> {
    let selected_libraries = install_roots
        .iter()
        .flat_map(|install_root| {
            roots
                .steam_libraries
                .values()
                .filter(move |steamapps| install_root.starts_with(steamapps.join("common")))
        })
        .cloned()
        .collect::<BTreeSet<_>>();
    let filename = format!("appmanifest_{steam_app_id}.acf");

    if selected_libraries.is_empty() {
        roots
            .steam_libraries
            .values()
            .map(|steamapps| steamapps.join(&filename))
            .find(|path| path.is_file())
    } else {
        selected_libraries
            .into_iter()
            .map(|steamapps| steamapps.join(&filename))
            .find(|path| path.is_file())
    }
}

fn steam_install_root_allowed(path: &Path) -> bool {
    steam_scope_path_allowed(path)
        && classify_observed_path(path).disposition == PathDisposition::FinalApplicationFile
}

fn steam_scope_path_allowed(path: &Path) -> bool {
    explicit_scope_path_allowed(path)
        && classify_observed_path(path).disposition != PathDisposition::RuntimeDependency
}

fn explicit_scope_path_allowed(path: &Path) -> bool {
    is_valid_index_path(path)
        && !matches!(
            classify_observed_path(path).disposition,
            PathDisposition::InstallStagingFile | PathDisposition::TemporaryFile
        )
        && !is_base_system_path(path)
        && !is_hard_volatile_root(path)
        && !is_noland_internal(path)
}

pub fn reconcile_app(agent: &StateAgent, app_id: &AppId) -> Result<usize> {
    let generation = agent.db.capture_generation(app_id)?;
    let associations = agent.db.associations_for_app(app_id)?;
    let dirty_roots = agent.db.list_dirty_roots(Some(app_id))?;
    let mut roots: Vec<PathBuf> = dirty_roots
        .iter()
        .map(|root| PathBuf::from(&root.canonical_root))
        .collect();
    let known_roots = known_app_roots(agent, app_id)?;
    if roots.is_empty() {
        roots = known_roots.clone();
        roots.extend(association_roots(&associations));
    }
    let install_roots = known_install_roots(agent, app_id)?;
    reconcile_roots(
        agent,
        app_id,
        roots,
        Some(6),
        &known_roots,
        &install_roots,
        true,
        None,
        generation,
        associations,
    )
}

/// Reconciles every root that can contain application content before a complete backup plans
/// candidates. Dirty evidence is deliberately retained until the backup commits successfully.
pub(crate) fn reconcile_app_for_complete_backup(
    agent: &StateAgent,
    app_id: &AppId,
) -> Result<usize> {
    reconcile_full_app(agent, app_id, false)
}

/// Explicit refresh must cover durable roots even when dirty events name paths inside
/// a private runtime namespace. Unlike an incremental scan, it has no depth limit.
pub(crate) fn reconcile_app_for_refresh(agent: &StateAgent, app_id: &AppId) -> Result<usize> {
    reconcile_full_app(agent, app_id, true)
}

fn reconcile_full_app(
    agent: &StateAgent,
    app_id: &AppId,
    clear_dirty_evidence: bool,
) -> Result<usize> {
    let generation = agent.db.capture_generation(app_id)?;
    let associations = agent.db.associations_for_app(app_id)?;
    if let Some(scope) = SteamBackupScope::load_with_associations(agent, app_id, &associations)? {
        let roots = scope.reconciliation_roots();
        return reconcile_roots(
            agent,
            app_id,
            roots.clone(),
            None,
            &roots,
            scope.install_roots(),
            clear_dirty_evidence,
            Some(&scope),
            generation,
            associations,
        );
    }

    let mut roots = agent
        .db
        .list_dirty_roots(Some(app_id))?
        .into_iter()
        .map(|root| PathBuf::from(root.canonical_root))
        .collect::<Vec<_>>();
    let known_roots = known_app_roots(agent, app_id)?;
    roots.extend(known_roots.iter().cloned());
    roots.extend(association_roots(&associations));
    let install_roots = known_install_roots(agent, app_id)?;
    reconcile_roots(
        agent,
        app_id,
        roots,
        None,
        &known_roots,
        &install_roots,
        clear_dirty_evidence,
        None,
        generation,
        associations,
    )
}

/// Rebuilds the authoritative index for final application content only. Installer staging,
/// user-state associations, and runtime prefixes are deliberately outside this scan.
pub(crate) fn reconcile_install_roots(agent: &StateAgent, app_id: &AppId) -> Result<usize> {
    let generation = agent.db.capture_generation(app_id)?;
    let associations = agent.db.associations_for_app(app_id)?;
    let strict_scope = SteamBackupScope::load_with_associations(agent, app_id, &associations)?;
    let install_roots = strict_scope
        .as_ref()
        .map(|scope| scope.install_roots().to_vec())
        .unwrap_or(known_install_roots(agent, app_id)?);
    let found = reconcile_roots(
        agent,
        app_id,
        install_roots.clone(),
        None,
        &install_roots,
        &install_roots,
        false,
        strict_scope.as_ref(),
        generation,
        associations,
    )?;

    let scanned_roots = install_roots
        .iter()
        .map(|root| root.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    agent
        .db
        .finish_reconciliation(app_id, generation, Some(&scanned_roots))?;
    Ok(found)
}

fn known_app_roots(agent: &StateAgent, app_id: &AppId) -> Result<Vec<PathBuf>> {
    Ok(agent
        .db
        .known_roots(Some(app_id))?
        .into_iter()
        .filter_map(|(_, _, path)| {
            let path = PathBuf::from(path);
            is_durable_root(&path).then_some(path)
        })
        .collect())
}

pub(crate) fn known_install_roots(agent: &StateAgent, app_id: &AppId) -> Result<Vec<PathBuf>> {
    Ok(agent
        .db
        .known_roots(Some(app_id))?
        .into_iter()
        .filter_map(|(_, kind, path)| {
            let path = PathBuf::from(path);
            (kind == "install" && is_durable_root(&path)).then_some(path)
        })
        .collect())
}

fn is_durable_root(path: &Path) -> bool {
    !matches!(
        classify_observed_path(path).disposition,
        PathDisposition::InstallStagingFile | PathDisposition::TemporaryFile
    )
}

fn association_roots(associations: &[(PathRecord, PathAssociation)]) -> Vec<PathBuf> {
    associations
        .iter()
        .filter_map(|(record, association)| reconciliation_root(record, association))
        .collect()
}

fn reconcile_roots(
    agent: &StateAgent,
    app_id: &AppId,
    roots: Vec<PathBuf>,
    max_depth: Option<usize>,
    known_roots: &[PathBuf],
    install_roots: &[PathBuf],
    clear_dirty_evidence: bool,
    strict_scope: Option<&SteamBackupScope>,
    generation: i64,
    associations: Vec<(PathRecord, PathAssociation)>,
) -> Result<usize> {
    noland_state_core::metrics::Metrics::inc(&agent.metrics.reconciliations_total);
    let preserve_trusted = !agent
        .db
        .list_dirty_apps()?
        .iter()
        .any(|dirty| dirty.app_id == *app_id && dirty.requires_reconciliation)
        && !agent
            .db
            .list_dirty_roots(Some(app_id))?
            .iter()
            .any(|root| root.requires_reconciliation);
    let existing = associations
        .into_iter()
        .map(|(record, association)| (record.canonical_path, association))
        .collect::<HashMap<_, _>>();
    let explicit_roots = existing
        .iter()
        .filter_map(|(path, association)| {
            association
                .evidence
                .iter()
                .any(|e| e.kind == EvidenceKind::ExplicitUserBinding)
                .then(|| PathBuf::from(path))
        })
        .collect::<HashSet<_>>();
    let is_explicit = |path: &Path| {
        path.ancestors()
            .any(|ancestor| explicit_roots.contains(ancestor))
    };
    let known_roots = known_roots.iter().cloned().collect::<HashSet<_>>();
    let known_ancestors = known_roots
        .iter()
        .flat_map(|root| root.ancestors().map(Path::to_path_buf))
        .collect::<HashSet<_>>();
    let install_roots = install_roots.iter().cloned().collect::<HashSet<_>>();
    let state_roots = agent
        .db
        .known_roots(Some(app_id))?
        .into_iter()
        .filter(|(_, kind, _)| matches!(kind.as_str(), "state" | "user-state" | "config" | "save"))
        .map(|(_, _, root)| PathBuf::from(root))
        .collect::<HashSet<_>>();
    let is_known_state = |path: &Path| {
        path.ancestors()
            .find(|ancestor| state_roots.contains(*ancestor) || install_roots.contains(*ancestor))
            .is_some_and(|ancestor| state_roots.contains(ancestor))
    };
    let allowed = |path: &Path| {
        let reaches_known = known_ancestors.contains(path)
            || path
                .ancestors()
                .any(|ancestor| known_roots.contains(ancestor));
        !agent.config.paths.is_internal(path)
            && !is_tracking_excluded(path, reaches_known, Some(&agent.config.home))
            && (!looks_like_cache_tree(path) || reaches_known)
            && strict_scope.is_none_or(|scope| scope.can_traverse(path))
    };
    let policy = |path: &Path| {
        if is_explicit(path) {
            ScanPolicy::Explicit
        } else if is_known_state(path) {
            ScanPolicy::State
        } else if path
            .ancestors()
            .any(|ancestor| install_roots.contains(ancestor))
        {
            ScanPolicy::Install
        } else {
            ScanPolicy::General
        }
    };
    let roots = minimal_scan_roots(roots, max_depth, &allowed, &policy);
    let logical_roots = agent.roots.lock().clone();
    let mut entries = Vec::with_capacity(512);
    let mut file_states = Vec::with_capacity(512);
    let mut found = 0;
    let now = Utc::now();
    let result = (|| -> Result<usize> {
        let mut walk = Walk::new(roots, max_depth, allowed);
        while let Some(entry) = walk.next() {
            let walkdir_lite::Entry { path, metadata } = entry?;
            let in_known_app_root = path
                .ancestors()
                .any(|ancestor| known_roots.contains(ancestor));
            if is_tracking_excluded(&path, in_known_app_root, Some(&agent.config.home))
                || (looks_like_cache(&path) && !in_known_app_root)
                || looks_like_lock_or_socket(&path)
                || strict_scope.is_some_and(|scope| !scope.contains(&path))
            {
                continue;
            }
            let canonical = path
                .to_str()
                .expect("walker validates index keys")
                .to_owned();
            let previous = existing.get(&canonical);
            let path_id = previous.map_or(0, |association| association.path_id);
            let logical = logical_roots.classify(&path);
            let file_type = if metadata.is_dir() {
                FileType::Directory
            } else if metadata.file_type().is_symlink() {
                FileType::Symlink
            } else if metadata.is_file() {
                FileType::File
            } else {
                continue;
            };
            #[cfg(unix)]
            use std::os::unix::fs::MetadataExt;
            let record = PathRecord {
                path_id,
                canonical_path: canonical,
                logical_root: logical.as_ref().map(|l| l.logical_root.as_token()),
                relative_path: logical.as_ref().map(|l| l.relative_path.clone()),
                file_type: Some(file_type.as_str().into()),
                #[cfg(unix)]
                inode: Some(metadata.ino() as i64),
                #[cfg(not(unix))]
                inode: None,
                #[cfg(unix)]
                mount_id: Some(metadata.dev() as i64),
                #[cfg(not(unix))]
                mount_id: None,
                size: Some(metadata.len() as i64),
                mtime_ns: metadata_mtime_ns(&metadata),
                #[cfg(unix)]
                mode: Some(metadata.mode() as i64),
                #[cfg(not(unix))]
                mode: None,
                #[cfg(unix)]
                uid: Some(metadata.uid() as i64),
                #[cfg(not(unix))]
                uid: None,
                #[cfg(unix)]
                gid: Some(metadata.gid() as i64),
                #[cfg(not(unix))]
                gid: None,
                content_hash: None,
                last_scanned_at: Some(now.timestamp()),
            };
            if let Some(logical) = logical.as_ref() {
                file_states.push(FileStateRecord {
                    app_id: app_id.clone(),
                    logical_root: logical.logical_root.as_token(),
                    relative_path: logical.relative_path.clone(),
                    canonical_path: Some(record.canonical_path.clone()),
                    file_type,
                    size: metadata.len(),
                    mtime_ns: record.mtime_ns.unwrap_or(0),
                    inode: record.inode.and_then(|value| u64::try_from(value).ok()),
                    mount_id: record.mount_id.and_then(|value| u64::try_from(value).ok()),
                    mode: record.mode.and_then(|value| u32::try_from(value).ok()),
                    content_hash: record.content_hash.clone(),
                    trust: FileStateTrust::VerifyRequired,
                    last_seen_at: now,
                    last_hashed_at: None,
                });
            }
            let install_root = path
                .ancestors()
                .find(|ancestor| install_roots.contains(*ancestor));
            let explicit_content = is_explicit(&path);
            let user_state = is_known_state(&path)
                || install_root.map_or_else(
                    || looks_like_user_state(&path),
                    |root| looks_like_user_state_in_install(&path, root),
                );
            let mut association = previous.cloned().unwrap_or_else(|| PathAssociation {
                app_id: app_id.clone(),
                path_id,
                confidence: CONF_REPEATED,
                evidence: vec![Evidence::new(EvidenceKind::ReconciliationDelta)],
                persistence_class: if user_state {
                    PersistenceClass::PersistentState
                } else {
                    PersistenceClass::Unknown
                },
                semantic_role: SemanticRole::Unknown,
                first_seen_at: now,
                last_seen_at: now,
            });
            if explicit_content {
                association.confidence = association.confidence.max(CONF_EXPLICIT);
                add_evidence(&mut association, EvidenceKind::ExplicitUserBinding);
                if !is_restored_app_content(&association) || user_state {
                    association.persistence_class = PersistenceClass::PersistentState;
                }
            } else if install_root.is_some() {
                association.confidence = association.confidence.max(CONF_DEPENDENCY);
                add_evidence(&mut association, EvidenceKind::KnownAppRoot);
                association.persistence_class = if user_state {
                    PersistenceClass::PersistentState
                } else {
                    add_evidence(&mut association, EvidenceKind::ReadOnlyDependency);
                    PersistenceClass::ReconstructableApp
                };
            } else if in_known_app_root {
                add_evidence(&mut association, EvidenceKind::KnownAppRoot);
            }
            association.semantic_role = infer_semantic_role(&path, association.persistence_class);
            association.last_seen_at = now;
            entries.push((record, association, previous.cloned()));
            found += 1;
            if entries.len() == 512 {
                agent.db.upsert_scanned_paths(&entries)?;
                agent
                    .db
                    .reconcile_file_states(&file_states, preserve_trusted)?;
                entries.clear();
                file_states.clear();
            }
        }
        agent.db.upsert_scanned_paths(&entries)?;
        agent
            .db
            .reconcile_file_states(&file_states, preserve_trusted)?;
        if walk.truncated {
            agent.db.mark_dirty(app_id, None, true)?;
        } else if clear_dirty_evidence {
            agent.db.finish_reconciliation(app_id, generation, None)?;
        }
        Ok(found)
    })();
    if result.is_err() {
        // Partial inventory must remain uncertain even if this scan started clean.
        agent.db.mark_dirty(app_id, None, true)?;
    }
    result
}

fn add_evidence(association: &mut PathAssociation, kind: EvidenceKind) {
    if !association
        .evidence
        .iter()
        .any(|evidence| evidence.kind == kind)
    {
        association.evidence.push(Evidence::new(kind));
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScanPolicy {
    General,
    Install,
    State,
    Explicit,
}

fn minimal_scan_roots(
    roots: Vec<PathBuf>,
    max_depth: Option<usize>,
    allowed: &impl Fn(&Path) -> bool,
    policy: &impl Fn(&Path) -> ScanPolicy,
) -> Vec<PathBuf> {
    let mut roots = roots
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    roots.sort_by_key(|root| root.components().count());
    let mut selected: Vec<PathBuf> = Vec::new();
    let mut policies = HashMap::new();
    for root in roots {
        // A finite ancestor scan cannot cover the descendant root's depth budget.
        // Never collapse across a policy boundary or a pruned intermediate tree.
        let root_policy = policy(&root);
        let covered = is_valid_index_path(&root)
            && max_depth.is_none()
            && root.ancestors().skip(1).any(|parent| {
                policies.get(parent) == Some(&root_policy)
                    && root
                        .ancestors()
                        .take_while(|path| *path != parent)
                        .all(allowed)
                    && allowed(parent)
            });
        if !covered {
            policies.insert(root.clone(), root_policy);
            selected.push(root);
        }
    }
    selected
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
    if association
        .evidence
        .iter()
        .any(|evidence| evidence.kind == EvidenceKind::ExplicitUserBinding)
    {
        return Some(path.to_path_buf());
    }
    if association.persistence_class == PersistenceClass::Ephemeral
        || association.persistence_class == PersistenceClass::BaseImage
        || !looks_like_user_state(path)
    {
        return None;
    }
    if record.file_type.as_deref() == Some("directory") {
        Some(path.to_path_buf())
    } else {
        path.parent().map(Path::to_path_buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentConfig;
    use uuid::Uuid;

    fn test_root(label: &str) -> PathBuf {
        std::env::current_dir()
            .expect("test working directory should be available")
            .join("target/noland-state-agent-tests")
            .join(format!("reconcile-{label}-{}", Uuid::new_v4()))
    }

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
    fn root_collapse_respects_policy_pruning_and_depth_budget() {
        let root = PathBuf::from("/srv/Game");
        let assets = root.join("romfs/deep");
        let explicit = root.join("custom/data");
        let nested = root.join("pruned/nested");
        let roots = vec![
            assets,
            explicit.clone(),
            root.clone(),
            nested.clone(),
            root.clone(),
        ];
        let allowed = |path: &Path| path != root.join("pruned");
        let policy = |path: &Path| {
            if path.starts_with(&explicit) {
                ScanPolicy::Explicit
            } else {
                ScanPolicy::Install
            }
        };
        let collapsed = minimal_scan_roots(roots.clone(), None, &allowed, &policy);
        assert_eq!(collapsed.len(), 3);
        assert!(collapsed.contains(&root));
        assert!(collapsed.contains(&explicit));
        assert!(collapsed.contains(&nested));
        assert_eq!(
            minimal_scan_roots(roots, Some(6), &allowed, &policy).len(),
            4
        );
    }

    #[test]
    fn overlapping_roots_open_each_directory_once_and_emit_each_path_once() {
        use std::cell::RefCell;
        thread_local! {
            static OPENED: RefCell<HashMap<PathBuf, usize>> = RefCell::new(HashMap::new());
        }
        fn counted_read(path: &Path) -> std::io::Result<std::fs::ReadDir> {
            OPENED.with(|opened| *opened.borrow_mut().entry(path.to_path_buf()).or_default() += 1);
            std::fs::read_dir(path)
        }
        let root = test_root("overlap-once");
        std::fs::create_dir_all(root.join("sub/deep")).unwrap();
        std::fs::write(root.join("sub/deep/asset.bin"), b"asset").unwrap();
        let entries = Walk::new(
            vec![root.clone(), root.join("sub"), root.join("sub/deep")],
            None,
            |_| true,
        )
        .with_read_directory(counted_read)
        .collect::<Result<Vec<_>>>()
        .unwrap();
        assert_eq!(entries.len(), 4);
        assert_eq!(
            entries
                .iter()
                .map(|entry| &entry.path)
                .collect::<BTreeSet<_>>()
                .len(),
            4
        );
        OPENED.with(|opened| assert!(opened.borrow().values().all(|count| *count == 1)));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn excluded_subtree_is_pruned_before_open_and_directory_errors_are_reported() {
        fn guarded_read(path: &Path) -> std::io::Result<std::fs::ReadDir> {
            assert_ne!(
                path.file_name().and_then(|name| name.to_str()),
                Some("cache"),
                "excluded directory opened"
            );
            if path.file_name().and_then(|name| name.to_str()) == Some("unreadable") {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected unreadable directory",
                ));
            }
            std::fs::read_dir(path)
        }
        let root = test_root("prune-before-descent");
        std::fs::create_dir_all(root.join("cache/deep")).unwrap();
        std::fs::write(root.join("cache/deep/ignored.bin"), b"ignored").unwrap();
        std::fs::write(root.join("asset.bin"), b"asset").unwrap();
        let entries = Walk::new(vec![root.clone()], None, |path| !looks_like_cache(path))
            .with_read_directory(guarded_read)
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(entries.len(), 2);
        std::fs::create_dir(root.join("unreadable")).unwrap();
        let error = Walk::new(vec![root.clone()], None, |path| !looks_like_cache(path))
            .with_read_directory(guarded_read)
            .collect::<Result<Vec<_>>>()
            .err()
            .unwrap();
        assert!(error.to_string().contains("unreadable"));
        assert!(error.to_string().contains("scan "));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn finite_walk_keeps_deeper_nested_roots_and_reports_truncation() {
        let root = test_root("bounded");
        std::fs::create_dir_all(root.join("a/b/c")).unwrap();
        std::fs::write(root.join("a/b/c/asset.bin"), b"asset").unwrap();
        let mut walk = Walk::new(vec![root.clone(), root.join("a/b")], Some(2), |_| true);
        let mut paths = BTreeSet::new();
        for entry in walk.by_ref() {
            assert!(paths.insert(entry.unwrap().path));
        }
        assert!(paths.contains(&root.join("a/b/c/asset.bin")));
        assert!(walk.truncated);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_indexed_as_links_without_following_or_bypassing_them() {
        use std::os::unix::fs::symlink;
        let root = test_root("symlinks");
        std::fs::create_dir_all(root.join("install")).unwrap();
        std::fs::create_dir_all(root.join("outside/sub")).unwrap();
        std::fs::write(root.join("outside/sub/secret.dat"), b"outside").unwrap();
        symlink(root.join("outside"), root.join("install/link")).unwrap();
        symlink("missing", root.join("install/dangling")).unwrap();
        let entries = Walk::new(vec![root.join("install")], None, |_| true)
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.metadata.file_type().is_symlink())
                .count(),
            2
        );
        assert!(
            Walk::new(vec![root.join("install/link/sub")], None, |_| true)
                .collect::<Result<Vec<_>>>()
                .is_err()
        );
        assert!(
            Walk::new(vec![root.join("install/link/missing")], None, |_| true)
                .collect::<Result<Vec<_>>>()
                .unwrap()
                .is_empty()
        );
        assert!(
            Walk::new(vec![root.join("install/../outside")], None, |_| true)
                .collect::<Result<Vec<_>>>()
                .is_err()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_refresh_keeps_reconciliation_uncertainty() {
        let root = test_root("failed-refresh");
        let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
        let app = AppIdentity::new(AppId::desktop("uncertain"), "Uncertain");
        agent.db.upsert_app(&app).unwrap();
        agent
            .db
            .add_known_root(
                &app.app_id,
                "install",
                &root.join("install/../outside").to_string_lossy(),
            )
            .unwrap();
        assert!(reconcile_app_for_refresh(&agent, &app.app_id).is_err());
        assert!(agent
            .db
            .list_dirty_apps()
            .unwrap()
            .iter()
            .any(|dirty| dirty.app_id == app.app_id && dirty.requires_reconciliation));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn restored_device_identity_stays_trusted_until_device_changes() {
        use std::os::unix::fs::MetadataExt;
        let root = test_root("restored-device");
        let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
        let install = agent.config.home.join("Games/Hookshot");
        let file = install.join("asset.bin");
        std::fs::create_dir_all(&install).unwrap();
        std::fs::write(&file, b"restored bytes").unwrap();
        let app = AppIdentity::new(AppId::desktop("restored-device"), "Restored Device");
        agent.db.upsert_app(&app).unwrap();
        agent
            .db
            .add_known_root(&app.app_id, "install", &install.to_string_lossy())
            .unwrap();
        let logical = agent.roots.lock().classify(&file).unwrap();
        let metadata = std::fs::metadata(&file).unwrap();
        let restored = FileStateRecord {
            app_id: app.app_id.clone(),
            logical_root: logical.logical_root.as_token(),
            relative_path: logical.relative_path.clone(),
            canonical_path: Some(file.to_string_lossy().into_owned()),
            file_type: FileType::File,
            size: metadata.len(),
            mtime_ns: metadata_mtime_ns(&metadata).unwrap(),
            inode: Some(metadata.ino()),
            mount_id: Some(metadata.dev()),
            mode: Some(metadata.mode()),
            content_hash: Some("verified-restored-hash".into()),
            trust: FileStateTrust::Trusted,
            last_seen_at: Utc::now(),
            last_hashed_at: Some(Utc::now()),
        };
        let association = noland_state_db::RestoredPathAssociationInput {
            canonical_path: file.to_string_lossy().into_owned(),
            persistence_class: PersistenceClass::ReconstructableApp,
            semantic_role: SemanticRole::AppContent,
        };
        agent
            .db
            .commit_restored_paths(
                &app.app_id,
                &[association],
                std::slice::from_ref(&restored),
                Some(agent.db.capture_generation(&app.app_id).unwrap()),
                |_| true,
            )
            .unwrap();
        reconcile_app_for_complete_backup(&agent, &app.app_id).unwrap();
        let mut indexed = agent
            .db
            .get_file_state(&app.app_id, &restored.logical_root, &restored.relative_path)
            .unwrap()
            .unwrap();
        assert_eq!(indexed.mount_id, Some(metadata.dev()));
        assert_eq!(indexed.trust, FileStateTrust::Trusted);
        assert_eq!(indexed.content_hash, restored.content_hash);
        assert!(indexed.fast_identity_matches(
            metadata.len(),
            restored.mtime_ns,
            Some(metadata.ino()),
            Some(metadata.dev())
        ));
        assert_eq!(
            agent
                .db
                .get_path_by_canonical(&file.to_string_lossy())
                .unwrap()
                .unwrap()
                .mount_id,
            Some(metadata.dev() as i64)
        );
        let restored_path = agent
            .db
            .get_path_by_canonical(&file.to_string_lossy())
            .unwrap()
            .unwrap();
        let restored_association = agent
            .db
            .associations_for_path(restored_path.path_id)
            .unwrap()
            .remove(0);
        assert_eq!(
            restored_association.persistence_class,
            PersistenceClass::ReconstructableApp
        );
        assert_eq!(restored_association.semantic_role, SemanticRole::AppContent);
        indexed.mount_id = Some(metadata.dev() ^ 1);
        agent.db.upsert_file_state(&indexed).unwrap();
        reconcile_app_for_complete_backup(&agent, &app.app_id).unwrap();
        let changed = agent
            .db
            .get_file_state(&app.app_id, &restored.logical_root, &restored.relative_path)
            .unwrap()
            .unwrap();
        assert_eq!(changed.mount_id, Some(metadata.dev()));
        assert_eq!(changed.trust, FileStateTrust::VerifyRequired);
        assert!(changed.content_hash.is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_refresh_keeps_dirty_evidence() {
        use std::os::unix::fs::PermissionsExt;
        let root = test_root("unreadable-refresh");
        let install = root.join("install");
        let unreadable = install.join("unreadable");
        std::fs::create_dir_all(&unreadable).unwrap();
        let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
        let app = AppIdentity::new(AppId::desktop("unreadable"), "Unreadable");
        agent.db.upsert_app(&app).unwrap();
        agent
            .db
            .add_known_root(&app.app_id, "install", &install.to_string_lossy())
            .unwrap();
        agent
            .db
            .mark_dirty_root(&app.app_id, &install.to_string_lossy(), None, true)
            .unwrap();
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o0)).unwrap();
        // Privileged test runners may bypass DAC; the deterministic walker test
        // above still covers permission-error propagation on those runners.
        if std::fs::read_dir(&unreadable).is_err() {
            assert!(reconcile_app_for_refresh(&agent, &app.app_id).is_err());
            assert_eq!(
                agent.db.list_dirty_roots(Some(&app.app_id)).unwrap().len(),
                1
            );
            assert!(agent
                .db
                .list_dirty_apps()
                .unwrap()
                .iter()
                .any(|dirty| dirty.requires_reconciliation));
        }
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn steam_explicit_nested_root_survives_pruning_of_runtime_ancestor() {
        let root = test_root("steam-nested-explicit");
        let install = root.join("steamapps/common/Game");
        let runtime = install.join("compatibilitytools.d/runtime");
        let explicit = runtime.join("custom-state");
        std::fs::create_dir_all(&explicit).unwrap();
        std::fs::write(explicit.join("progress.dat"), b"state").unwrap();
        std::fs::write(runtime.join("runtime.bin"), b"runtime").unwrap();
        std::fs::write(install.join("asset.bin"), b"asset").unwrap();
        let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
        let app = AppIdentity::new(AppId::steam(8081), "Game");
        agent.db.upsert_app(&app).unwrap();
        agent
            .db
            .add_known_root(&app.app_id, "install", &install.to_string_lossy())
            .unwrap();
        let path_id = agent.db.upsert_path(&explicit.to_string_lossy()).unwrap();
        let mut bound = association(PersistenceClass::PersistentState);
        bound.app_id = app.app_id.clone();
        bound.path_id = path_id;
        bound
            .evidence
            .push(Evidence::new(EvidenceKind::ExplicitUserBinding));
        agent.db.upsert_association(&bound).unwrap();
        reconcile_app_for_complete_backup(&agent, &app.app_id).unwrap();
        let state = agent
            .db
            .get_path_by_canonical(&explicit.join("progress.dat").to_string_lossy())
            .unwrap()
            .unwrap();
        let state_assoc = agent
            .db
            .associations_for_path(state.path_id)
            .unwrap()
            .remove(0);
        assert_eq!(
            state_assoc.persistence_class,
            PersistenceClass::PersistentState
        );
        assert!(state_assoc
            .evidence
            .iter()
            .any(|e| e.kind == EvidenceKind::ExplicitUserBinding));
        assert!(agent
            .db
            .get_path_by_canonical(&runtime.join("runtime.bin").to_string_lossy())
            .unwrap()
            .is_none());
        let scope = SteamBackupScope::load(&agent, &app.app_id)
            .unwrap()
            .unwrap();
        assert!(!scope.contains(&install.join("../OtherGame/asset.bin")));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn complete_reconciliation_scans_all_known_roots_and_retains_dirty_evidence() {
        let root = test_root("complete-roots");
        let install = root.join("install");
        let deep_install_file = install
            .join("one/two/three/four/five/six/seven/eight")
            .join("asset.pak");
        let dirty_state = root.join("user-state");
        let state_file = dirty_state.join("save.dat");
        std::fs::create_dir_all(deep_install_file.parent().unwrap()).unwrap();
        std::fs::create_dir_all(&dirty_state).unwrap();
        std::fs::write(&deep_install_file, b"deep asset").unwrap();
        std::fs::write(&state_file, b"save").unwrap();

        let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
        let app = AppIdentity::new(AppId::desktop("test-game"), "Test Game");
        let app_id = app.app_id.clone();
        agent.db.upsert_app(&app).unwrap();
        agent
            .db
            .add_known_root(&app_id, "install", install.to_string_lossy().as_ref())
            .unwrap();
        agent
            .db
            .add_known_root(&app_id, "state", dirty_state.to_string_lossy().as_ref())
            .unwrap();
        agent
            .db
            .mark_dirty_root(&app_id, dirty_state.to_string_lossy().as_ref(), None, true)
            .unwrap();

        reconcile_app_for_complete_backup(&agent, &app_id).unwrap();

        for path in [&deep_install_file, &state_file] {
            let record = agent
                .db
                .get_path_by_canonical(path.to_string_lossy().as_ref())
                .unwrap()
                .expect("known-root file should be reconciled");
            assert!(agent
                .db
                .associations_for_path(record.path_id)
                .unwrap()
                .iter()
                .any(|association| association.app_id == app_id));
        }
        assert_eq!(agent.db.list_dirty_roots(Some(&app_id)).unwrap().len(), 1);

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn steam_complete_reconciliation_includes_app_scoped_prefixes_and_caches() {
        let root = test_root("steam-complete-scope");
        let home = root.join("home");
        let steamapps = home.join(".local/share/Steam/steamapps");
        let install = steamapps.join("common/Game");
        let install_file = install.join("data/content.pak");
        let compatdata = steamapps.join("compatdata/8080/pfx");
        let compatdata_file = compatdata.join("drive_c/windows/system32/runtime.dll");
        let proton = steamapps.join("common/Proton 9.0");
        let proton_file = proton.join("proton");
        let other_game = steamapps.join("common/Other Game");
        let other_file = other_game.join("other.bin");
        let shader_cache = steamapps.join("shadercache/8080");
        let shader_file = shader_cache.join("cache.bin");
        let explicit_config =
            compatdata.join("drive_c/users/steamuser/AppData/Roaming/Game/config");
        let explicit_file = explicit_config.join("settings.json");
        let explicit_save = compatdata.join("drive_c/users/steamuser/Saved Games/Game/slot1");
        let explicit_save_file = explicit_save.join("progress.dat");
        for file in [
            &install_file,
            &compatdata_file,
            &proton_file,
            &other_file,
            &shader_file,
            &explicit_file,
            &explicit_save_file,
        ] {
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, b"data").unwrap();
        }

        let mut config = AgentConfig::isolated(root.clone());
        config.home = home;
        let agent = StateAgent::boot(config).unwrap();
        let app = AppIdentity::new(AppId::steam(8080), "Game");
        let app_id = app.app_id.clone();
        agent.db.upsert_app(&app).unwrap();
        agent
            .roots
            .lock()
            .steam_libraries
            .insert("selected".into(), steamapps.clone());
        let broad_common_root = steamapps.join("common");
        for (kind, path) in [
            ("install", &install),
            ("install", &broad_common_root),
            ("proton", &compatdata),
            ("install", &proton),
        ] {
            agent
                .db
                .add_known_root(&app_id, kind, &path.to_string_lossy())
                .unwrap();
        }
        for explicit_root in [&explicit_config, &explicit_save] {
            let explicit_id = agent
                .db
                .upsert_path(&explicit_root.to_string_lossy())
                .unwrap();
            agent
                .db
                .upsert_association(&PathAssociation {
                    app_id: app_id.clone(),
                    path_id: explicit_id,
                    confidence: CONF_EXPLICIT,
                    evidence: vec![Evidence::new(EvidenceKind::ExplicitUserBinding)],
                    persistence_class: PersistenceClass::PersistentState,
                    semantic_role: SemanticRole::UserState,
                    first_seen_at: Utc::now(),
                    last_seen_at: Utc::now(),
                })
                .unwrap();
        }
        for dirty_root in [&compatdata, &proton, &other_game, &shader_cache] {
            agent
                .db
                .mark_dirty_root(&app_id, &dirty_root.to_string_lossy(), None, true)
                .unwrap();
        }

        reconcile_app_for_complete_backup(&agent, &app_id).unwrap();

        for included in [
            &install_file,
            &explicit_file,
            &explicit_save_file,
            &compatdata_file,
            &shader_file,
        ] {
            let record = agent
                .db
                .get_path_by_canonical(&included.to_string_lossy())
                .unwrap()
                .unwrap_or_else(|| panic!("missing in-scope path {}", included.display()));
            if included == &explicit_file || included == &explicit_save_file {
                let association = agent
                    .db
                    .associations_for_path(record.path_id)
                    .unwrap()
                    .into_iter()
                    .find(|association| association.app_id == app_id)
                    .expect("explicit descendant should remain associated with the app");
                assert!(association
                    .evidence
                    .iter()
                    .any(|evidence| evidence.kind == EvidenceKind::ExplicitUserBinding));
                assert_eq!(
                    association.persistence_class,
                    PersistenceClass::PersistentState
                );
            }
        }
        for excluded in [&proton_file, &other_file] {
            assert!(
                agent
                    .db
                    .get_path_by_canonical(&excluded.to_string_lossy())
                    .unwrap()
                    .is_none(),
                "indexed out-of-scope path {}",
                excluded.display()
            );
        }
        let scope = SteamBackupScope::load(&agent, &app_id).unwrap().unwrap();
        assert!(scope.contains(&explicit_file));
        assert!(scope.contains(&explicit_save_file));
        assert!(scope.contains(&compatdata_file));
        assert!(!scope.contains(Path::new("/usr/lib/libc.so.6")));

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn install_reconciliation_ignores_staging_and_runtime_roots_and_corrects_content() {
        let root = test_root("install-only");
        let install = root.join("steamapps/common/Game");
        let install_file = install.join("data/content.pak");
        let staging = root.join("steamapps/downloading/8080");
        let staging_file = staging.join("data/content.pak");
        let runtime = root.join("steamapps/compatdata/8080/pfx");
        let runtime_file = runtime.join("drive_c/runtime.dll");
        for file in [&install_file, &staging_file, &runtime_file] {
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, b"data").unwrap();
        }

        let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
        let app = AppIdentity::new(AppId::steam(8080), "Game");
        let app_id = app.app_id.clone();
        agent.db.upsert_app(&app).unwrap();
        for (kind, path) in [
            ("install", &install),
            ("install", &staging),
            ("proton", &runtime),
        ] {
            agent
                .db
                .add_known_root(&app_id, kind, &path.to_string_lossy())
                .unwrap();
        }
        let path_id = agent
            .db
            .upsert_path(&install_file.to_string_lossy())
            .unwrap();
        agent
            .db
            .upsert_association(&PathAssociation {
                app_id: app_id.clone(),
                path_id,
                confidence: CONF_REPEATED,
                evidence: vec![Evidence::new(EvidenceKind::ReconciliationDelta)],
                persistence_class: PersistenceClass::PersistentState,
                semantic_role: SemanticRole::UserState,
                first_seen_at: Utc::now(),
                last_seen_at: Utc::now(),
            })
            .unwrap();

        reconcile_install_roots(&agent, &app_id).unwrap();

        let association = agent
            .db
            .associations_for_path(path_id)
            .unwrap()
            .into_iter()
            .find(|association| association.app_id == app_id)
            .unwrap();
        assert_eq!(
            association.persistence_class,
            PersistenceClass::ReconstructableApp
        );
        assert_eq!(association.semantic_role, SemanticRole::AppContent);
        assert!(agent
            .db
            .get_path_by_canonical(&staging_file.to_string_lossy())
            .unwrap()
            .is_none());
        assert!(agent
            .db
            .get_path_by_canonical(&runtime_file.to_string_lossy())
            .unwrap()
            .is_none());

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn reconciliation_does_not_index_managed_sunshine_state() {
        let root = test_root("sunshine-exclusion");
        let home = root.join("home");
        let sunshine = home.join(".config/sunshine/sunshine.conf");
        let app_state = home.join(".config/example-app/settings.toml");
        std::fs::create_dir_all(sunshine.parent().unwrap()).unwrap();
        std::fs::create_dir_all(app_state.parent().unwrap()).unwrap();
        std::fs::write(&sunshine, b"managed").unwrap();
        std::fs::write(&app_state, b"user state").unwrap();

        let mut config = AgentConfig::isolated(root.clone());
        config.home = home.clone();
        let agent = StateAgent::boot(config).unwrap();
        let app = AppIdentity::new(AppId::desktop("example-app"), "Example App");
        let app_id = app.app_id.clone();
        agent.db.upsert_app(&app).unwrap();
        agent
            .db
            .mark_dirty_root(&app_id, home.to_string_lossy().as_ref(), None, true)
            .unwrap();

        reconcile_app(&agent, &app_id).unwrap();

        assert!(agent
            .db
            .get_path_by_canonical(app_state.to_string_lossy().as_ref())
            .unwrap()
            .is_some());
        assert!(agent
            .db
            .get_path_by_canonical(sunshine.to_string_lossy().as_ref())
            .unwrap()
            .is_none());
        std::fs::remove_dir_all(root).ok();
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
    use std::collections::HashMap;
    use std::fs::{Metadata, ReadDir};
    use std::io;
    use std::path::{Path, PathBuf};

    use noland_state_core::{is_valid_index_path, Result, StateError};

    pub struct Entry {
        pub path: PathBuf,
        pub metadata: Metadata,
    }

    pub struct Walk<F> {
        stack: Vec<(PathBuf, usize, bool)>,
        visited: HashMap<PathBuf, usize>,
        allowed: F,
        read_directory: fn(&Path) -> io::Result<ReadDir>,
        pub truncated: bool,
    }

    impl<F: Fn(&Path) -> bool> Walk<F> {
        pub fn new(roots: Vec<PathBuf>, max_depth: Option<usize>, allowed: F) -> Self {
            Self {
                stack: roots
                    .into_iter()
                    .rev()
                    .map(|root| (root, max_depth.unwrap_or(usize::MAX), true))
                    .collect(),
                visited: HashMap::new(),
                allowed,
                read_directory: |path| std::fs::read_dir(path),
                truncated: false,
            }
        }

        #[cfg(test)]
        pub fn with_read_directory(mut self, read: fn(&Path) -> io::Result<ReadDir>) -> Self {
            self.read_directory = read;
            self
        }

        fn visit(&mut self, path: PathBuf, remaining: usize, root: bool) -> Result<Option<Entry>> {
            if !is_valid_index_path(&path) {
                return Err(StateError::UnsafePath(path.to_string_lossy().into_owned()));
            }
            // Crucially, neither metadata nor read_dir is called for an excluded
            // subtree. Explicit nested roots remain separate traversal entries.
            if !(self.allowed)(&path)
                || self
                    .visited
                    .get(&path)
                    .is_some_and(|depth| *depth >= remaining)
            {
                return Ok(None);
            }
            let metadata = match std::fs::symlink_metadata(&path) {
                // Absent roots are ordinary deletion/stale namespace evidence.
                // Check the leaf first: a stale runtime namespace may have a
                // host alias (e.g. /home on macOS) among its ancestors.
                // Disappearance *during* a walk is an incomplete observation.
                Err(error) if root && error.kind() == io::ErrorKind::NotFound => return Ok(None),
                result => result.map_err(|error| scan_error(&path, error))?,
            };
            if root {
                // A separately queued nested root must not bypass a directory
                // symlink that the normal traversal would never follow.
                for ancestor in path.ancestors().skip(1) {
                    match std::fs::symlink_metadata(ancestor) {
                        Ok(metadata) if metadata.file_type().is_symlink() => {
                            return Err(StateError::UnsafePath(format!(
                                "scan root traverses symlink {}",
                                ancestor.display()
                            )));
                        }
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                        Err(error) => return Err(scan_error(ancestor, error)),
                        _ => {}
                    }
                }
            }
            if metadata.is_dir() {
                if remaining == 0 {
                    self.truncated = true;
                } else {
                    let entries =
                        (self.read_directory)(&path).map_err(|error| scan_error(&path, error))?;
                    for entry in entries {
                        let entry = entry.map_err(|error| scan_error(&path, error))?;
                        let next_depth = if remaining == usize::MAX {
                            usize::MAX
                        } else {
                            remaining - 1
                        };
                        self.stack.push((entry.path(), next_depth, false));
                    }
                }
            }
            let already_emitted = self.visited.insert(path.clone(), remaining).is_some();
            Ok((!already_emitted).then_some(Entry { path, metadata }))
        }
    }

    impl<F: Fn(&Path) -> bool> Iterator for Walk<F> {
        type Item = Result<Entry>;

        fn next(&mut self) -> Option<Self::Item> {
            while let Some((path, remaining, root)) = self.stack.pop() {
                match self.visit(path, remaining, root) {
                    Ok(Some(entry)) => return Some(Ok(entry)),
                    Ok(None) => {}
                    Err(error) => {
                        self.stack.clear();
                        return Some(Err(error));
                    }
                }
            }
            None
        }
    }

    fn scan_error(path: &Path, error: io::Error) -> StateError {
        StateError::Io(io::Error::new(
            error.kind(),
            format!("scan {}: {error}", path.display()),
        ))
    }
}
