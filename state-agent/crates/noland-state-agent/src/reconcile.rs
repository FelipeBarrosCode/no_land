use std::collections::BTreeSet;
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
    metadata_files: Vec<PathBuf>,
}

impl SteamBackupScope {
    pub(crate) fn load(agent: &StateAgent, app_id: &AppId) -> Result<Option<Self>> {
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
        let explicit_roots: Vec<PathBuf> = agent
            .db
            .associations_for_app(app_id)?
            .into_iter()
            .filter(|(_, association)| {
                association
                    .evidence
                    .iter()
                    .any(|evidence| evidence.kind == EvidenceKind::ExplicitUserBinding)
            })
            .map(|(record, _)| PathBuf::from(record.canonical_path))
            .filter(|root| explicit_scope_path_allowed(root))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        let metadata_files: Vec<PathBuf> =
            steam_appmanifest_path(steam_app_id, &agent.roots.lock(), &install_roots)
                .into_iter()
                .collect();

        Ok(Some(Self {
            install_roots,
            explicit_roots,
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
            .cloned()
            .collect()
    }

    pub(crate) fn contains(&self, path: &Path) -> bool {
        self.metadata_files.iter().any(|metadata| path == metadata)
            || (steam_scope_path_allowed(path)
                && self.install_roots.iter().any(|root| path.starts_with(root)))
            || self.contains_explicit(path)
    }

    fn contains_explicit(&self, path: &Path) -> bool {
        explicit_scope_path_allowed(path)
            && self
                .explicit_roots
                .iter()
                .any(|root| path.starts_with(root))
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
    !matches!(
        classify_observed_path(path).disposition,
        PathDisposition::InstallStagingFile | PathDisposition::TemporaryFile
    ) && !looks_like_cache(path)
        && !is_base_system_path(path)
        && !is_hard_volatile_root(path)
        && !is_noland_internal(path)
}

pub fn reconcile_app(agent: &StateAgent, app_id: &AppId) -> Result<usize> {
    let dirty_roots = agent.db.list_dirty_roots(Some(app_id))?;
    let mut roots: Vec<PathBuf> = dirty_roots
        .iter()
        .map(|root| PathBuf::from(&root.canonical_root))
        .collect();
    let known_roots = known_app_roots(agent, app_id)?;
    if roots.is_empty() {
        roots = known_roots.clone();
        roots.extend(association_roots(agent, app_id)?);
    }
    reconcile_roots(agent, app_id, roots, Some(6), &known_roots, &[], true, None)
}

/// Reconciles every root that can contain application content before a complete backup plans
/// candidates. Dirty evidence is deliberately retained until the backup commits successfully.
pub(crate) fn reconcile_app_for_complete_backup(
    agent: &StateAgent,
    app_id: &AppId,
) -> Result<usize> {
    if let Some(scope) = SteamBackupScope::load(agent, app_id)? {
        let roots = scope.reconciliation_roots();
        return reconcile_roots(
            agent,
            app_id,
            roots.clone(),
            None,
            &roots,
            scope.install_roots(),
            false,
            Some(&scope),
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
    roots.extend(association_roots(agent, app_id)?);
    let install_roots = known_install_roots(agent, app_id)?;
    reconcile_roots(
        agent,
        app_id,
        roots,
        None,
        &known_roots,
        &install_roots,
        false,
        None,
    )
}

/// Rebuilds the authoritative index for final application content only. Installer staging,
/// user-state associations, and runtime prefixes are deliberately outside this scan.
pub(crate) fn reconcile_install_roots(agent: &StateAgent, app_id: &AppId) -> Result<usize> {
    let strict_scope = SteamBackupScope::load(agent, app_id)?;
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
    )?;

    for root in &install_roots {
        agent.db.clear_dirty_root(app_id, &root.to_string_lossy())?;
    }
    if !agent
        .db
        .list_dirty_roots(Some(app_id))?
        .iter()
        .any(|root| root.requires_reconciliation)
    {
        agent.db.clear_reconciliation_required(app_id)?;
    }
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

fn association_roots(agent: &StateAgent, app_id: &AppId) -> Result<Vec<PathBuf>> {
    Ok(agent
        .db
        .associations_for_app(app_id)?
        .into_iter()
        .filter_map(|(record, association)| reconciliation_root(&record, &association))
        .collect())
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
) -> Result<usize> {
    noland_state_core::metrics::Metrics::inc(&agent.metrics.reconciliations_total);
    let roots = roots.into_iter().collect::<BTreeSet<_>>();
    let mut found = 0;
    let now = Utc::now();
    for root in roots {
        let root_reaches_known_path = known_roots
            .iter()
            .any(|known| root.starts_with(known) || known.starts_with(&root));
        if !root.exists()
            || agent.config.paths.is_internal(&root)
            || is_tracking_excluded(&root, root_reaches_known_path, Some(&agent.config.home))
        {
            continue;
        }
        let walk = max_depth
            .map(|depth| Walk::new(&root).max_depth(depth))
            .unwrap_or_else(|| Walk::new(&root));
        for path in walk {
            if strict_scope.is_some_and(|scope| !scope.contains(&path)) {
                continue;
            }
            let in_known_app_root = known_roots.iter().any(|root| path.starts_with(root));
            if agent.config.paths.is_internal(&path)
                || is_tracking_excluded(&path, in_known_app_root, Some(&agent.config.home))
                || looks_like_cache(&path)
                || looks_like_lock_or_socket(&path)
            {
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
            let install_content = install_roots.iter().any(|root| path.starts_with(root));
            let explicit_content = strict_scope.is_some_and(|scope| scope.contains_explicit(&path));
            if let Some(mut association) = existing
                .into_iter()
                .find(|association| association.app_id == *app_id)
            {
                if explicit_content {
                    association.confidence = association.confidence.max(CONF_EXPLICIT);
                    if !association
                        .evidence
                        .iter()
                        .any(|evidence| evidence.kind == EvidenceKind::ExplicitUserBinding)
                    {
                        association
                            .evidence
                            .push(Evidence::new(EvidenceKind::ExplicitUserBinding));
                    }
                    association.persistence_class = PersistenceClass::PersistentState;
                    association.semantic_role = SemanticRole::UserState;
                    association.last_seen_at = now;
                    agent.db.upsert_association(&association)?;
                } else if install_content {
                    association.confidence = association.confidence.max(CONF_DEPENDENCY);
                    if !association
                        .evidence
                        .iter()
                        .any(|evidence| evidence.kind == EvidenceKind::KnownAppRoot)
                    {
                        association
                            .evidence
                            .push(Evidence::new(EvidenceKind::KnownAppRoot));
                    }
                    association.persistence_class = PersistenceClass::ReconstructableApp;
                    association.semantic_role = SemanticRole::AppContent;
                    association.last_seen_at = now;
                    agent.db.upsert_association(&association)?;
                }
                found += 1;
                continue;
            }
            let (confidence, evidence, persistence_class, semantic_role) = if explicit_content {
                (
                    CONF_EXPLICIT,
                    vec![Evidence::new(EvidenceKind::ExplicitUserBinding)],
                    PersistenceClass::PersistentState,
                    SemanticRole::UserState,
                )
            } else if install_content {
                (
                    CONF_DEPENDENCY,
                    vec![
                        Evidence::new(EvidenceKind::KnownAppRoot),
                        Evidence::new(EvidenceKind::ReadOnlyDependency),
                    ],
                    PersistenceClass::ReconstructableApp,
                    SemanticRole::AppContent,
                )
            } else {
                let persistence_class = if looks_like_user_state(&path) {
                    PersistenceClass::PersistentState
                } else {
                    PersistenceClass::Unknown
                };
                let mut evidence = vec![Evidence::new(EvidenceKind::ReconciliationDelta)];
                if in_known_app_root {
                    evidence.push(Evidence::new(EvidenceKind::KnownAppRoot));
                }
                (
                    CONF_REPEATED,
                    evidence,
                    persistence_class,
                    infer_semantic_role(&path, persistence_class),
                )
            };
            agent.db.upsert_association(&PathAssociation {
                app_id: app_id.clone(),
                path_id,
                confidence,
                evidence,
                persistence_class,
                semantic_role,
                first_seen_at: now,
                last_seen_at: now,
            })?;
            found += 1;
        }
    }
    if clear_dirty_evidence {
        agent.db.clear_reconciliation_required(app_id)?;
        agent.db.clear_dirty_roots(app_id)?;
    }
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
    use crate::AgentConfig;

    fn test_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "noland-reconcile-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
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
    fn steam_complete_reconciliation_scans_only_install_and_explicit_roots() {
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

        for included in [&install_file, &explicit_file, &explicit_save_file] {
            let record = agent
                .db
                .get_path_by_canonical(&included.to_string_lossy())
                .unwrap()
                .unwrap_or_else(|| panic!("missing in-scope path {}", included.display()));
            if included != &install_file {
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
        for excluded in [&compatdata_file, &proton_file, &other_file, &shader_file] {
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
        assert!(!scope.contains(&compatdata_file));
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
    use std::path::{Path, PathBuf};

    pub struct Walk {
        stack: Vec<(PathBuf, usize)>,
        max_depth: usize,
    }

    impl Walk {
        pub fn new(root: &Path) -> Self {
            Self {
                stack: vec![(root.to_path_buf(), 0)],
                max_depth: usize::MAX,
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
            let is_directory = std::fs::symlink_metadata(&path)
                .is_ok_and(|metadata| metadata.file_type().is_dir());
            if is_directory && depth < self.max_depth {
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
