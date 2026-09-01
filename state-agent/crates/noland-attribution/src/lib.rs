//! Session correlation, installer transactions, and inspectable ownership.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::Utc;
use noland_discovery::{
    fallback_exe_identity, is_backup_candidate, resolve_identity_for_executable, SteamDiscovery,
};
use noland_observer::{self_excluded, ObserverHub};
use noland_state_core::*;
use noland_state_db::StateDb;
use uuid::Uuid;

const UNRESOLVED_TTL: Duration = Duration::from_secs(2);
const UNRESOLVED_LIMIT: usize = 256;

#[derive(Clone)]
enum PendingFilesystemFact {
    Legacy(FilesystemEvent),
    Ebpf(EbpfFilesystemFact),
}

struct PendingFilesystemEvent {
    fact: PendingFilesystemFact,
    queued_at: Instant,
}

#[derive(Debug, Clone, Copy)]
struct CgroupSessionBinding {
    root_pid: i32,
    dedicated: bool,
    ambiguous: bool,
}

pub struct AttributionEngine<'a> {
    pub db: &'a StateDb,
    pub roots: LogicalRootMap,
    pub agent_paths: AgentPaths,
    pub known_apps: Vec<AppIdentity>,
    pub steam: Option<SteamDiscovery>,
    cgroup_sessions: HashMap<u64, CgroupSessionBinding>,
    unresolved: VecDeque<PendingFilesystemEvent>,
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
            cgroup_sessions: HashMap::new(),
            unresolved: VecDeque::new(),
        }
    }

    pub fn ingest_process(&mut self, event: &ProcessEvent) -> Result<Option<AppSession>> {
        let session = self.ingest_process_inner(event)?;
        if event.kind != ProcessEventKind::Exit {
            self.retry_unresolved()?;
        }
        Ok(session)
    }

    pub fn ingest_ebpf_process(&mut self, fact: &EbpfProcessFact) -> Result<Option<AppSession>> {
        let event = fact.as_process_event();
        let session = self.ingest_process_inner(&event)?;
        if fact.cgroup_id != 0 {
            if fact.kind == ProcessEventKind::Exit {
                if let Some(session) = &session {
                    self.remove_cgroup_session(fact.cgroup_id, session.root_pid);
                }
            } else if let Some(session) = &session {
                self.record_cgroup_session(
                    fact.cgroup_id,
                    session.root_pid,
                    fact.cgroup.as_deref().is_some_and(is_dedicated_cgroup),
                );
            }
        }
        if fact.kind != ProcessEventKind::Exit {
            self.retry_unresolved()?;
        }
        Ok(session)
    }

    fn ingest_process_inner(&mut self, event: &ProcessEvent) -> Result<Option<AppSession>> {
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
                let existing = self.db.session_for_pid(event.pid)?;
                let parent = if existing.is_none() {
                    self.db.session_for_pid(event.ppid)?
                } else {
                    None
                };
                let inherited = existing.or(parent);
                let Some(exe) = event.executable.as_ref() else {
                    return Ok(inherited);
                };

                let identity = self.resolve_identity(exe, event);
                let identity_was_known = self
                    .known_apps
                    .iter()
                    .any(|known| known.app_id == identity.app_id);

                if let Some(inherited) = inherited {
                    if inherited.app_id == identity.app_id
                        || !identity_was_known
                        || !is_backup_candidate(&identity)
                    {
                        self.db.attach_pid(
                            inherited.session_id,
                            event.pid,
                            Some(event.ppid),
                            Some(&exe.to_string_lossy()),
                        )?;
                        return Ok(Some(inherited));
                    }
                    self.db.detach_pid(event.pid)?;
                }

                self.db.upsert_app(&identity)?;
                if !identity_was_known {
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

    pub fn ingest_fs(&mut self, event: &FilesystemEvent) -> Result<Option<PathAssociation>> {
        if self.event_is_excluded(event) {
            return Ok(None);
        }
        let Some(session) = self.db.session_for_pid(event.pid)? else {
            self.queue_unresolved(PendingFilesystemFact::Legacy(event.clone()));
            return Ok(None);
        };
        self.ingest_fs_for_session(event, &session, None, None)
    }

    pub fn ingest_ebpf_fs(&mut self, fact: &EbpfFilesystemFact) -> Result<Option<PathAssociation>> {
        if fact.io_result.is_some_and(|result| result < 0) {
            return Ok(None);
        }
        let event = fact.as_filesystem_event();
        if self.event_is_excluded(&event) {
            return Ok(None);
        }
        let Some(session) = self.session_for_ebpf_fs(fact)? else {
            self.queue_unresolved(PendingFilesystemFact::Ebpf(fact.clone()));
            return Ok(None);
        };
        self.ingest_fs_for_session(&event, &session, fact.inode, fact.device)
    }

    fn ingest_fs_for_session(
        &self,
        event: &FilesystemEvent,
        session: &AppSession,
        inode: Option<u64>,
        device: Option<u64>,
    ) -> Result<Option<PathAssociation>> {
        let observed_path = if event.kind == FsEventKind::Rename {
            event.second_path().unwrap_or(&event.path)
        } else {
            &event.path
        };
        let canonical = canonicalize_lossy(observed_path);
        let path_id = self.db.upsert_path(&canonical)?;
        let logical = self.roots.classify(Path::new(&canonical));
        let mut record = PathRecord {
            path_id,
            canonical_path: canonical.clone(),
            logical_root: logical.as_ref().map(|l| l.logical_root.as_token()),
            relative_path: logical.as_ref().map(|l| l.relative_path.clone()),
            file_type: Some("file".into()),
            inode: inode.and_then(|value| i64::try_from(value).ok()),
            mount_id: device.and_then(|value| i64::try_from(value).ok()),
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
                evidence.push(
                    Evidence::new(EvidenceKind::DirectCgroupCreate).with_detail(canonical.clone()),
                );
            }
            FsEventKind::Write | FsEventKind::Truncate => {
                evidence.push(
                    Evidence::new(EvidenceKind::DirectCgroupWrite).with_detail(canonical.clone()),
                );
            }
            FsEventKind::Rename => {
                let detail = event
                    .second_path()
                    .map(|target| format!("{} -> {}", event.path.display(), target.display()));
                let mut rename = Evidence::new(EvidenceKind::DirectCgroupRename);
                if let Some(detail) = detail {
                    rename = rename.with_detail(detail);
                }
                evidence.push(rename);
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
        let persistence_class =
            infer_initial_class(Path::new(&canonical), event.kind, in_known_root);
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

    fn event_is_excluded(&self, event: &FilesystemEvent) -> bool {
        let path = if event.kind == FsEventKind::Rename {
            event.second_path().unwrap_or(&event.path)
        } else {
            &event.path
        };
        self_excluded(path, &self.agent_paths) || is_hard_volatile_root(path)
    }

    fn session_for_ebpf_fs(&self, fact: &EbpfFilesystemFact) -> Result<Option<AppSession>> {
        let mut direct_session = None;
        for pid in [fact.tgid, fact.tid] {
            if pid != 0 {
                if let Some(session) = self.db.session_for_pid(pid)? {
                    direct_session = Some(session);
                    break;
                }
            }
        }

        if fact.cgroup_id != 0 {
            if let Some(binding) = self.cgroup_sessions.get(&fact.cgroup_id) {
                if !binding.ambiguous && (binding.dedicated || direct_session.is_none()) {
                    if let Some(session) = self.db.session_for_pid(binding.root_pid)? {
                        return Ok(Some(session));
                    }
                }
            }
        }
        if direct_session.is_some() {
            return Ok(direct_session);
        }
        if fact.ppid != 0 {
            return self.db.session_for_pid(fact.ppid);
        }
        Ok(None)
    }

    fn record_cgroup_session(&mut self, cgroup_id: u64, root_pid: i32, dedicated: bool) {
        self.cgroup_sessions
            .entry(cgroup_id)
            .and_modify(|binding| {
                if binding.root_pid != root_pid {
                    binding.ambiguous = true;
                    binding.dedicated = false;
                } else {
                    binding.dedicated |= dedicated;
                }
            })
            .or_insert(CgroupSessionBinding {
                root_pid,
                dedicated,
                ambiguous: false,
            });
    }

    fn remove_cgroup_session(&mut self, cgroup_id: u64, root_pid: i32) {
        if self
            .cgroup_sessions
            .get(&cgroup_id)
            .is_some_and(|binding| binding.root_pid == root_pid && !binding.ambiguous)
        {
            self.cgroup_sessions.remove(&cgroup_id);
        }
    }

    fn queue_unresolved(&mut self, fact: PendingFilesystemFact) {
        self.drop_expired_unresolved();
        if self.unresolved.len() == UNRESOLVED_LIMIT {
            self.unresolved.pop_front();
        }
        self.unresolved.push_back(PendingFilesystemEvent {
            fact,
            queued_at: Instant::now(),
        });
    }

    fn drop_expired_unresolved(&mut self) {
        while self
            .unresolved
            .front()
            .is_some_and(|pending| pending.queued_at.elapsed() > UNRESOLVED_TTL)
        {
            self.unresolved.pop_front();
        }
    }

    fn retry_unresolved(&mut self) -> Result<()> {
        self.drop_expired_unresolved();
        let mut remaining = VecDeque::new();
        while let Some(pending) = self.unresolved.pop_front() {
            let result = match &pending.fact {
                PendingFilesystemFact::Legacy(event) => self
                    .db
                    .session_for_pid(event.pid)?
                    .map(|session| self.ingest_fs_for_session(event, &session, None, None))
                    .transpose()?,
                PendingFilesystemFact::Ebpf(fact) => self
                    .session_for_ebpf_fs(fact)?
                    .map(|session| {
                        let event = fact.as_filesystem_event();
                        self.ingest_fs_for_session(&event, &session, fact.inode, fact.device)
                    })
                    .transpose()?,
            };
            if result.is_none() {
                remaining.push_back(pending);
            }
        }
        self.unresolved = remaining;
        Ok(())
    }

    pub fn unresolved_len(&self) -> usize {
        self.unresolved.len()
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

pub fn infer_initial_class(
    path: &Path,
    kind: FsEventKind,
    in_known_root: bool,
) -> PersistenceClass {
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

fn is_dedicated_cgroup(path: &str) -> bool {
    path.split('/').any(|component| component == "noland") && path.contains("/apps/")
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
            noland_observer::QueuedEvent::EbpfProcess(fact) => {
                engine.ingest_ebpf_process(&fact)?;
                n += 1;
            }
            noland_observer::QueuedEvent::EbpfFilesystem(fact) => {
                engine.ingest_ebpf_fs(&fact)?;
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
        let mut engine = AttributionEngine::new(&db, roots, paths);
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

    #[test]
    fn ebpf_attribution_prefers_cgroup_then_falls_back_to_session_pid() {
        let db = StateDb::open_in_memory().unwrap();
        let home = std::env::temp_dir().join(format!("noland-cgroup-{}", Uuid::new_v4()));
        std::fs::create_dir_all(home.join("state")).unwrap();
        let cgroup_path = home.join("state/cgroup.dat");
        let fallback_path = home.join("state/fallback.dat");
        std::fs::write(&cgroup_path, b"cgroup").unwrap();
        std::fs::write(&fallback_path, b"fallback").unwrap();

        let app_a = AppIdentity::new(AppId::desktop("app-a"), "App A");
        let app_b = AppIdentity::new(AppId::desktop("app-b"), "App B");
        db.upsert_app(&app_a).unwrap();
        db.upsert_app(&app_b).unwrap();
        let session_a = AppSession::new(app_a.app_id.clone(), 101, SessionSource::DesktopEntry);
        let session_b = AppSession::new(app_b.app_id.clone(), 202, SessionSource::DesktopEntry);
        db.insert_session(&session_a).unwrap();
        db.insert_session(&session_b).unwrap();

        let roots = LogicalRootMap::from_home(&home);
        let paths = AgentPaths::from_roots(home.join("agent-state"), home.join("agent-run"));
        let mut engine = AttributionEngine::new(&db, roots, paths);
        engine
            .ingest_ebpf_process(&EbpfProcessFact {
                kind: ProcessEventKind::Exec,
                tgid: 202,
                tid: 202,
                cgroup_id: 77,
                cgroup: Some("/noland/apps/app-b/session".into()),
                source: ObservationSource::Ebpf,
                ..EbpfProcessFact::default()
            })
            .unwrap();

        let cgroup_assoc = engine
            .ingest_ebpf_fs(&EbpfFilesystemFact {
                kind: FsEventKind::Write,
                tgid: 101,
                tid: 101,
                cgroup_id: 77,
                path: cgroup_path,
                inode: Some(123),
                device: Some(45),
                source: ObservationSource::Ebpf,
                ..EbpfFilesystemFact::default()
            })
            .unwrap()
            .unwrap();
        assert_eq!(cgroup_assoc.app_id, app_b.app_id);

        let fallback_assoc = engine
            .ingest_ebpf_fs(&EbpfFilesystemFact {
                kind: FsEventKind::Write,
                tgid: 101,
                tid: 101,
                cgroup_id: 999,
                path: fallback_path,
                source: ObservationSource::Ebpf,
                ..EbpfFilesystemFact::default()
            })
            .unwrap()
            .unwrap();
        assert_eq!(fallback_assoc.app_id, app_a.app_id);
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn known_app_exec_splits_from_shared_desktop_session() {
        let db = StateDb::open_in_memory().unwrap();
        let home = std::env::temp_dir().join(format!("noland-shared-cgroup-{}", Uuid::new_v4()));
        std::fs::create_dir_all(home.join("state")).unwrap();
        let iso = home.join("state/vice-city.iso");
        let desktop_file = home.join("state/desktop-state");
        std::fs::write(&iso, b"game").unwrap();
        std::fs::write(&desktop_file, b"desktop").unwrap();

        let desktop = AppIdentity::new(AppId::desktop("desktop-shell"), "Desktop Shell");
        let mut pcsx2 = AppIdentity::new(AppId::desktop("pcsx2"), "PCSX2");
        let pcsx2_exe = home.join("PCSX2.AppImage");
        pcsx2.canonical_executable = Some(pcsx2_exe.clone());
        db.upsert_app(&desktop).unwrap();
        db.upsert_app(&pcsx2).unwrap();
        let desktop_session = AppSession::new(
            desktop.app_id.clone(),
            10,
            SessionSource::ExecutableDiscovery,
        );
        db.insert_session(&desktop_session).unwrap();

        let roots = LogicalRootMap::from_home(&home);
        let paths = AgentPaths::from_roots(home.join("agent-state"), home.join("agent-run"));
        let mut engine = AttributionEngine::new(&db, roots, paths);
        let shared_cgroup = "/system.slice/noland-desktop.service";

        engine
            .ingest_ebpf_process(&EbpfProcessFact {
                kind: ProcessEventKind::Fork,
                tgid: 20,
                tid: 20,
                ppid: 10,
                cgroup_id: 77,
                cgroup: Some(shared_cgroup.into()),
                source: ObservationSource::Ebpf,
                ..EbpfProcessFact::default()
            })
            .unwrap();
        let pcsx2_session = engine
            .ingest_ebpf_process(&EbpfProcessFact {
                kind: ProcessEventKind::Exec,
                tgid: 20,
                tid: 20,
                ppid: 10,
                cgroup_id: 77,
                cgroup: Some(shared_cgroup.into()),
                executable: Some(pcsx2_exe),
                comm: Some("PCSX2".into()),
                source: ObservationSource::Ebpf,
                ..EbpfProcessFact::default()
            })
            .unwrap()
            .unwrap();
        assert_eq!(pcsx2_session.app_id, pcsx2.app_id);
        assert_eq!(
            db.session_for_pid(20).unwrap().unwrap().app_id,
            pcsx2.app_id
        );

        let game_assoc = engine
            .ingest_ebpf_fs(&EbpfFilesystemFact {
                kind: FsEventKind::Read,
                tgid: 20,
                tid: 20,
                ppid: 10,
                cgroup_id: 77,
                path: iso,
                source: ObservationSource::Ebpf,
                ..EbpfFilesystemFact::default()
            })
            .unwrap()
            .unwrap();
        assert_eq!(game_assoc.app_id, pcsx2.app_id);

        let desktop_assoc = engine
            .ingest_ebpf_fs(&EbpfFilesystemFact {
                kind: FsEventKind::Write,
                tgid: 10,
                tid: 10,
                cgroup_id: 77,
                path: desktop_file,
                source: ObservationSource::Ebpf,
                ..EbpfFilesystemFact::default()
            })
            .unwrap()
            .unwrap();
        assert_eq!(desktop_assoc.app_id, desktop.app_id);
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn unresolved_fs_fact_retries_after_its_process_fact() {
        let db = StateDb::open_in_memory().unwrap();
        let home = std::env::temp_dir().join(format!("noland-queued-{}", Uuid::new_v4()));
        std::fs::create_dir_all(home.join("state")).unwrap();
        let save = home.join("state/queued-save.dat");
        std::fs::write(&save, b"queued").unwrap();
        let roots = LogicalRootMap::from_home(&home);
        let paths = AgentPaths::from_roots(home.join("agent-state"), home.join("agent-run"));
        let mut engine = AttributionEngine::new(&db, roots, paths);

        assert!(engine
            .ingest_ebpf_fs(&EbpfFilesystemFact {
                kind: FsEventKind::Write,
                tgid: 404,
                tid: 405,
                cgroup_id: 88,
                path: save.clone(),
                source: ObservationSource::Ebpf,
                ..EbpfFilesystemFact::default()
            })
            .unwrap()
            .is_none());
        assert_eq!(engine.unresolved_len(), 1);

        let session = engine
            .ingest_ebpf_process(&EbpfProcessFact {
                kind: ProcessEventKind::Exec,
                tgid: 404,
                tid: 404,
                ppid: 1,
                cgroup_id: 88,
                executable: Some(PathBuf::from("/opt/queued-app")),
                comm: Some("queued-app".into()),
                source: ObservationSource::Ebpf,
                ..EbpfProcessFact::default()
            })
            .unwrap()
            .unwrap();
        assert_eq!(engine.unresolved_len(), 0);
        let path_id = db.upsert_path(&canonicalize_lossy(&save)).unwrap();
        assert!(db
            .associations_for_path(path_id)
            .unwrap()
            .iter()
            .any(|association| association.app_id == session.app_id));
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn rename_attributes_the_second_path() {
        let db = StateDb::open_in_memory().unwrap();
        let home = std::env::temp_dir().join(format!("noland-rename-{}", Uuid::new_v4()));
        std::fs::create_dir_all(home.join("state")).unwrap();
        let old_path = home.join("state/old-name");
        let second_path = home.join("state/new-name");

        let app = AppIdentity::new(AppId::desktop("rename-app"), "Rename App");
        db.upsert_app(&app).unwrap();
        let session = AppSession::new(app.app_id.clone(), 303, SessionSource::DesktopEntry);
        db.insert_session(&session).unwrap();
        let roots = LogicalRootMap::from_home(&home);
        let paths = AgentPaths::from_roots(home.join("agent-state"), home.join("agent-run"));
        let mut engine = AttributionEngine::new(&db, roots, paths);

        let assoc = engine
            .ingest_ebpf_fs(&EbpfFilesystemFact {
                kind: FsEventKind::Rename,
                tgid: 303,
                tid: 303,
                path: old_path,
                second_path: Some(second_path.clone()),
                source: ObservationSource::Ebpf,
                ..EbpfFilesystemFact::default()
            })
            .unwrap()
            .unwrap();
        let expected_path_id = db.upsert_path(&canonicalize_lossy(&second_path)).unwrap();
        assert_eq!(assoc.path_id, expected_path_id);
        std::fs::remove_dir_all(home).ok();
    }
}
