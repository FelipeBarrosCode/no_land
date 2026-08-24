//! Session correlation, installer transactions, and inspectable ownership.

use std::path::{Path, PathBuf};

use chrono::Utc;
use noland_discovery::{fallback_exe_identity, resolve_identity_for_executable, SteamDiscovery};
use noland_observer::{self_excluded, ObserverHub};
use noland_state_core::*;
use noland_state_db::StateDb;
use uuid::Uuid;

pub struct AttributionEngine<'a> {
    pub db: &'a StateDb,
    pub roots: LogicalRootMap,
    pub agent_paths: AgentPaths,
    pub known_apps: Vec<AppIdentity>,
    pub steam: Option<SteamDiscovery>,
}

impl<'a> AttributionEngine<'a> {
    pub fn new(db: &'a StateDb, roots: LogicalRootMap, agent_paths: AgentPaths) -> Self {
        let known_apps = db.list_apps().unwrap_or_default();
        Self {
            db,
            roots,
            agent_paths,
            known_apps,
            steam: None,
        }
    }

    pub fn ingest_process(&mut self, event: &ProcessEvent) -> Result<Option<AppSession>> {
        match event.kind {
            ProcessEventKind::Fork | ProcessEventKind::Clone => {
                if let Some(parent) = self.db.session_for_pid(event.ppid)? {
                    self.db.attach_pid(
                        parent.session_id,
                        event.pid,
                        Some(event.ppid),
                        event
                            .executable
                            .as_ref()
                            .map(|p| p.to_string_lossy())
                            .as_deref(),
                    )?;
                    return Ok(Some(parent));
                }
                Ok(None)
            }
            ProcessEventKind::Exec => {
                if let Some(existing) = self.db.session_for_pid(event.pid)? {
                    return Ok(Some(existing));
                }
                if let Some(parent) = self.db.session_for_pid(event.ppid)? {
                    self.db.attach_pid(
                        parent.session_id,
                        event.pid,
                        Some(event.ppid),
                        event
                            .executable
                            .as_ref()
                            .map(|p| p.to_string_lossy())
                            .as_deref(),
                    )?;
                    return Ok(Some(parent));
                }
                let Some(exe) = event.executable.as_ref() else {
                    return Ok(None);
                };
                let identity = self.resolve_identity(exe, event);
                self.db.upsert_app(&identity)?;
                if !self.known_apps.iter().any(|a| a.app_id == identity.app_id) {
                    self.known_apps.push(identity.clone());
                }
                let source = infer_session_source(&identity, exe);
                let mut session = AppSession::new(identity.app_id.clone(), event.pid, source);
                session.identity_confidence = identity.identity_confidence;
                if let Some(cgroup) = &event.cgroup {
                    if !cgroup.is_empty() {
                        session.cgroup_path = cgroup.clone();
                    }
                }
                self.db.insert_session(&session)?;
                Ok(Some(session))
            }
            ProcessEventKind::Exit => {
                if let Some(session) = self.db.session_for_pid(event.pid)? {
                    if session.root_pid == event.pid {
                        self.db.end_session(session.session_id)?;
                    } else {
                        self.db.detach_pid(event.pid)?;
                    }
                    return Ok(Some(session));
                }
                self.db.detach_pid(event.pid)?;
                Ok(None)
            }
        }
    }

    pub fn ingest_fs(&self, event: &FilesystemEvent) -> Result<Option<PathAssociation>> {
        if self_excluded(&event.path, &self.agent_paths) || is_hard_volatile_root(&event.path) {
            return Ok(None);
        }
        let session = self.db.session_for_pid(event.pid)?;
        let Some(session) = session else {
            return Ok(None);
        };
        let canonical = canonicalize_lossy(&event.path);
        let path_id = self.db.upsert_path(&canonical)?;
        let logical = self.roots.classify(Path::new(&canonical));
        let mut record = PathRecord {
            path_id,
            canonical_path: canonical.clone(),
            logical_root: logical.as_ref().map(|l| l.logical_root.as_token()),
            relative_path: logical.as_ref().map(|l| l.relative_path.clone()),
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
        };
        if let Ok(meta) = std::fs::metadata(&canonical) {
            record.size = Some(meta.len() as i64);
            record.file_type = Some(if meta.is_dir() {
                "directory".into()
            } else if meta.file_type().is_symlink() {
                "symlink".into()
            } else {
                "file".into()
            });
        }
        self.db.update_path_meta(path_id, &record)?;

        let mut evidence = Vec::new();
        let in_known_root = self.path_in_known_root(&session.app_id, Path::new(&canonical))?;
        match event.kind {
            FsEventKind::Create | FsEventKind::Mkdir => {
                evidence.push(Evidence::new(EvidenceKind::DirectCgroupCreate).with_detail(canonical.clone()));
            }
            FsEventKind::Write | FsEventKind::Truncate => {
                evidence.push(Evidence::new(EvidenceKind::DirectCgroupWrite).with_detail(canonical.clone()));
            }
            FsEventKind::Rename => {
                evidence.push(Evidence::new(EvidenceKind::DirectCgroupRename));
            }
            FsEventKind::Unlink | FsEventKind::Rmdir => {
                evidence.push(Evidence::new(EvidenceKind::DirectCgroupDelete));
            }
            FsEventKind::Read | FsEventKind::Open | FsEventKind::Mmap | FsEventKind::Execve => {
                evidence.push(Evidence::new(EvidenceKind::ReadOnlyDependency));
            }
            _ => {}
        }
        if in_known_root {
            evidence.push(Evidence::new(EvidenceKind::KnownAppRoot));
        }
        if looks_like_user_state(Path::new(&canonical)) {
            evidence.push(Evidence::new(EvidenceKind::KnownUserStateRoot));
        }
        if self.is_steam_path(Path::new(&canonical)) {
            evidence.push(Evidence::new(EvidenceKind::SteamMetadata));
        }
        if self.is_proton_path(Path::new(&canonical)) {
            evidence.push(Evidence::new(EvidenceKind::ProtonPrefix));
        }
        if self.is_wine_path(Path::new(&canonical)) {
            evidence.push(Evidence::new(EvidenceKind::WinePrefix));
        }

        if let Some(existing) = self
            .db
            .associations_for_path(path_id)?
            .into_iter()
            .find(|a| a.app_id == session.app_id)
        {
            let mutation_count = existing
                .evidence
                .iter()
                .filter(|e| e.kind.is_mutation())
                .count()
                + event.kind.is_mutation() as usize;
            evidence.extend(existing.evidence);
            if mutation_count >= 2 {
                evidence.push(Evidence::new(EvidenceKind::RepeatedSessionUse));
            }
        }

        let (confidence, _breakdown) = score_evidence(&evidence);
        let persistence_class = infer_initial_class(Path::new(&canonical), event.kind, in_known_root);
        let semantic_role = crate::infer_role(Path::new(&canonical), persistence_class);
        let now = Utc::now();
        let assoc = PathAssociation {
            app_id: session.app_id.clone(),
            path_id,
            confidence,
            evidence,
            persistence_class,
            semantic_role,
            first_seen_at: now,
            last_seen_at: now,
        };
        self.db.upsert_association(&assoc)?;
        if event.kind.is_mutation() && persistence_class != PersistenceClass::Ephemeral {
            self.db.mark_dirty(&session.app_id, Some(path_id), false)?;
        }
        Ok(Some(assoc))
    }

    pub fn bind_manual(&self, app_id: &AppId, path: &Path) -> Result<PathAssociation> {
        let canonical = canonicalize_lossy(path);
        let path_id = self.db.upsert_path(&canonical)?;
        self.db.add_known_root(app_id, "manual", &canonical)?;
        let now = Utc::now();
        let assoc = PathAssociation {
            app_id: app_id.clone(),
            path_id,
            confidence: CONF_EXPLICIT,
            evidence: vec![Evidence::new(EvidenceKind::ExplicitUserBinding)],
            persistence_class: PersistenceClass::PersistentState,
            semantic_role: SemanticRole::UserState,
            first_seen_at: now,
            last_seen_at: now,
        };
        self.db.upsert_association(&assoc)?;
        self.db.mark_dirty(app_id, Some(path_id), true)?;
        Ok(assoc)
    }

    pub fn exclude_path(&self, path: &Path, app_id: Option<&AppId>) -> Result<()> {
        self.db
            .set_path_policy(&canonicalize_lossy(path), app_id, "exclude")
    }

    fn resolve_identity(&self, exe: &Path, event: &ProcessEvent) -> AppIdentity {
        if let Some(steam) = self.identify_steam(exe, event) {
            return steam;
        }
        if let Some(found) = resolve_identity_for_executable(&self.known_apps, exe) {
            return found;
        }
        if let Some(comm) = &event.comm {
            if let Some(found) = self
                .known_apps
                .iter()
                .find(|app| names_match(&app.display_name, comm))
            {
                return found.clone();
            }
        }
        fallback_exe_identity(exe)
    }

    fn identify_steam(&self, exe: &Path, event: &ProcessEvent) -> Option<AppIdentity> {
        let text = format!(
            "{} {}",
            exe.display(),
            event.comm.as_deref().unwrap_or_default()
        );
        if let Some(steam) = &self.steam {
            for app in &steam.apps {
                if text.contains(&app.app_id.to_string())
                    || exe.starts_with(&app.install_dir)
                    || app
                        .prefix
                        .as_ref()
                        .is_some_and(|prefix| exe.starts_with(prefix))
                {
                    return Some(app.to_identity());
                }
            }
        }
        None
    }

    fn path_in_known_root(&self, app_id: &AppId, path: &Path) -> Result<bool> {
        for (_, _, root) in self.db.known_roots(Some(app_id))? {
            if path.starts_with(&root) {
                return Ok(true);
            }
        }
        if let Some(steam) = &self.steam {
            for app in &steam.apps {
                if AppId::steam(app.app_id) == *app_id
                    && (path.starts_with(&app.install_dir)
                        || app.prefix.as_ref().is_some_and(|p| path.starts_with(p)))
                {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    fn is_steam_path(&self, path: &Path) -> bool {
        let text = path.to_string_lossy();
        text.contains("/steamapps/") || text.contains("/.steam/")
    }

    fn is_proton_path(&self, path: &Path) -> bool {
        path.to_string_lossy().contains("/compatdata/")
    }

    fn is_wine_path(&self, path: &Path) -> bool {
        let text = path.to_string_lossy();
        text.contains("/.wine") || text.contains("/drive_c/") || text.contains("/bottles/")
    }
}

pub fn infer_initial_class(path: &Path, kind: FsEventKind, in_known_root: bool) -> PersistenceClass {
    if looks_like_cache(path) || looks_like_lock_or_socket(path) || is_hard_volatile_root(path) {
        return PersistenceClass::Ephemeral;
    }
    if looks_like_os_or_lib(path) && !kind.is_mutation() {
        return PersistenceClass::BaseImage;
    }
    if in_known_root && kind.is_mutation() && !looks_like_user_state(path) {
        return PersistenceClass::ReconstructableApp;
    }
    if looks_like_user_state(path) || kind.is_mutation() {
        return PersistenceClass::PersistentState;
    }
    PersistenceClass::Unknown
}

fn infer_role(path: &Path, class: PersistenceClass) -> SemanticRole {
    crate::policy_role(path, class)
}

fn policy_role(path: &Path, class: PersistenceClass) -> SemanticRole {
    noland_state_core::infer_semantic_role(path, class)
}

fn names_match(a: &str, b: &str) -> bool {
    noland_discovery::names_equivalent(a, b)
}

fn infer_session_source(identity: &AppIdentity, exe: &Path) -> SessionSource {
    if identity.steam_app_id.is_some() || identity.launcher == Some(LauncherKind::Steam) {
        return SessionSource::Steam;
    }
    if identity.desktop_entry_id.is_some() {
        return SessionSource::DesktopEntry;
    }
    match identity.launcher {
        Some(LauncherKind::Proton) => SessionSource::Proton,
        Some(LauncherKind::Wine) => SessionSource::Wine,
        Some(LauncherKind::Bottles) => SessionSource::Bottles,
        _ => {
            let name = exe
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if name.contains("steam") {
                SessionSource::Steam
            } else if name.contains("wine") {
                SessionSource::Wine
            } else if name.contains("proton") {
                SessionSource::Proton
            } else {
                SessionSource::ExecutableDiscovery
            }
        }
    }
}

pub fn canonicalize_lossy(path: &Path) -> String {
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

pub fn process_hub_events(engine: &mut AttributionEngine<'_>, hub: &ObserverHub) -> Result<usize> {
    let mut n = 0;
    for event in hub.drain() {
        match event {
            noland_observer::QueuedEvent::Process(ev) => {
                engine.ingest_process(&ev)?;
                n += 1;
            }
            noland_observer::QueuedEvent::Filesystem(ev) => {
                engine.ingest_fs(&ev)?;
                n += 1;
            }
        }
    }
    Ok(n)
}

pub fn start_installer(
    db: &StateDb,
    app_id: AppId,
    session_id: Option<Uuid>,
    roots: Vec<PathBuf>,
    ty: InstallTransactionType,
) -> Result<InstallerTransaction> {
    let tx = InstallerTransaction {
        transaction_id: Uuid::new_v4(),
        app_id: app_id.clone(),
        session_id,
        started_at: Utc::now(),
        ended_at: None,
        candidate_roots: roots.clone(),
        transaction_type: ty,
        confidence: 0.9,
    };
    db.insert_installer(&tx)?;
    for root in roots {
        db.add_known_root(&app_id, "install", &root.to_string_lossy())?;
    }
    Ok(tx)
}

pub fn finish_installer(db: &StateDb, tx: &InstallerTransaction) -> Result<()> {
    db.finish_installer(tx.transaction_id)?;
    db.mark_dirty(&tx.app_id, None, true)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use noland_state_core::metrics::Metrics;
    use std::sync::Arc;

    #[test]
    fn write_is_owned_read_is_not() {
        let db = StateDb::open_in_memory().unwrap();
        let home = std::env::temp_dir().join(format!("noland-attr-{}", Uuid::new_v4()));
        std::fs::create_dir_all(home.join(".local/share/example-game")).unwrap();
        let save = home.join(".local/share/example-game/save.db");
        std::fs::write(&save, b"hello").unwrap();
        let lib = PathBuf::from("/usr/lib/libc.so.6");
        let roots = LogicalRootMap::from_home(&home);
        let paths = AgentPaths::from_roots(home.join("state"), home.join("run"));
        let app = AppIdentity::new(AppId::desktop("example-game"), "Example Game");
        db.upsert_app(&app).unwrap();
        let session = AppSession::new(app.app_id.clone(), 42, SessionSource::DesktopEntry);
        db.insert_session(&session).unwrap();
        let engine = AttributionEngine::new(&db, roots, paths);
        let write = FilesystemEvent {
            kind: FsEventKind::Write,
            pid: 42,
            path: save.clone(),
            dest_path: None,
            at: Utc::now(),
            sampled: false,
        };
        let assoc = engine.ingest_fs(&write).unwrap().unwrap();
        assert!(assoc.confidence >= CONF_DIRECT_OUTSIDE_ROOT);
        assert_ne!(assoc.persistence_class, PersistenceClass::Ephemeral);
        let read = FilesystemEvent {
            kind: FsEventKind::Read,
            pid: 42,
            path: lib,
            dest_path: None,
            at: Utc::now(),
            sampled: false,
        };
        let dep = engine.ingest_fs(&read).unwrap().unwrap();
        assert!(dep.confidence <= CONF_DEPENDENCY + 0.05);
        std::fs::remove_dir_all(home).ok();
        let _ = Metrics::default();
        let _ = Arc::new(());
    }
}
