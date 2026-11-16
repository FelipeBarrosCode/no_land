use super::*;
use chrono::Utc;
use noland_observer::{fs_event, process_exec, ProcessGenerationProof, QueuedEvent};
use noland_state_core::*;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

const PID: i32 = 41;

struct Fixture {
    root: PathBuf,
    agent: StateAgent,
    proc_root: PathBuf,
    apps: [AppIdentity; 3],
    saves: [PathBuf; 3],
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root = crate::index_test_support::root(label);
        let agent = StateAgent::boot(crate::AgentConfig::isolated(root.clone())).unwrap();
        let apps = ["Alpha", "Beta", "Gamma"].map(|name| {
            let executable = agent.config.home.join("Games").join(name).join("game");
            std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
            std::fs::write(&executable, b"never launched").unwrap();
            let mut app = AppIdentity::new(AppId::desktop(&format!("proof-{name}")), name);
            app.canonical_executable = Some(executable);
            app.launcher = Some(LauncherKind::Native);
            agent.db.upsert_app(&app).unwrap();
            app
        });
        let saves = ["Alpha", "Beta", "Gamma"].map(|name| {
            let path = agent
                .config
                .home
                .join(".config")
                .join(name)
                .join("save.dat");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"old!").unwrap();
            path
        });
        for (app, save) in apps.iter().zip(&saves) {
            let path_id = agent.db.upsert_path(save.to_str().unwrap()).unwrap();
            agent
                .db
                .upsert_association(&PathAssociation {
                    app_id: app.app_id.clone(),
                    path_id,
                    confidence: 1.0,
                    evidence: vec![Evidence::new(EvidenceKind::ExplicitUserBinding)],
                    persistence_class: PersistenceClass::PersistentState,
                    semantic_role: SemanticRole::UserState,
                    first_seen_at: Utc::now(),
                    last_seen_at: Utc::now(),
                })
                .unwrap();
            let logical = agent.roots.lock().classify(save).unwrap();
            agent
                .db
                .upsert_file_state(&FileStateRecord {
                    app_id: app.app_id.clone(),
                    logical_root: logical.logical_root.as_token(),
                    relative_path: logical.relative_path,
                    canonical_path: Some(save.to_string_lossy().into_owned()),
                    file_type: FileType::File,
                    size: 4,
                    mtime_ns: 1,
                    inode: None,
                    mount_id: None,
                    mode: None,
                    content_hash: Some("retained-hash".into()),
                    trust: FileStateTrust::Trusted,
                    last_seen_at: Utc::now(),
                    last_hashed_at: Some(Utc::now()),
                })
                .unwrap();
        }
        let proc_root = root.join("proc");
        let fixture = Self {
            root,
            agent,
            proc_root,
            apps,
            saves,
        };
        fixture.process(0, 1);
        fixture
    }

    fn process(&self, app: usize, generation: u64) {
        let root = self.proc_root.join(PID.to_string());
        std::fs::create_dir_all(root.join("root")).unwrap();
        std::fs::create_dir_all(root.join("ns")).unwrap();
        if !root.join("ns/mnt").is_symlink() {
            symlink("mnt:[1041]", root.join("ns/mnt")).unwrap();
        }
        if root.join("exe").is_symlink() {
            std::fs::remove_file(root.join("exe")).unwrap();
        }
        symlink(
            self.apps[app].canonical_executable.as_ref().unwrap(),
            root.join("exe"),
        )
        .unwrap();
        let mut fields = vec!["0".to_string(); 20];
        fields[0] = "S".into();
        fields[19] = generation.to_string();
        std::fs::write(
            root.join("stat"),
            format!("{PID} (game) {}", fields.join(" ")),
        )
        .unwrap();
    }

    fn event(&self, app: usize) -> (ProcessEvent, ProcessGenerationProof) {
        let event = process_exec(
            PID,
            1,
            self.apps[app].canonical_executable.as_ref().unwrap(),
        );
        let proof = ProcessGenerationProof::capture_from_proc(
            &self.proc_root,
            PID,
            event.at,
            event.executable.as_deref(),
        )
        .unwrap();
        (event, proof)
    }

    fn fs_proof(&self, at: chrono::DateTime<Utc>) -> ProcessGenerationProof {
        ProcessGenerationProof::capture_from_proc(&self.proc_root, PID, at, None).unwrap()
    }

    fn recover(&self, event: &ProcessEvent, proof: &ProcessGenerationProof) {
        self.agent
            .collector
            .lock()
            .with_engine(self.agent.attribution_engine(), |engine| {
                engine.ingest_recovered_process(event, Some(proof))?;
                Ok(())
            })
            .unwrap();
    }

    fn no_foreign(&self, app: usize, save: usize) {
        assert!(self
            .agent
            .db
            .associations_for_app(&self.apps[app].app_id)
            .unwrap()
            .iter()
            .all(|(path, _)| Path::new(&path.canonical_path) != self.saves[save]));
        assert!(self
            .agent
            .db
            .pending_app_mutations(&self.apps[app].app_id, 100)
            .unwrap()
            .iter()
            .all(|mutation| Path::new(&mutation.path) != self.saves[save]));
    }

    fn finish(self) {
        drop(self.agent);
        std::fs::remove_dir_all(self.root).unwrap();
    }
}

#[test]
fn fs_arriving_during_procfs_scan_cannot_borrow_the_newer_recovery_mapping() {
    for stamped in [false, true] {
        for generation in [1, 2] {
            let fixture = Fixture::new("collector-procfs-overtake");
            let old = fs_event(FsEventKind::Write, PID, &fixture.saves[0]);
            let old_proof = stamped.then(|| fixture.fs_proof(old.at));
            fixture
                .agent
                .db
                .insert_session(&AppSession::new(
                    fixture.apps[0].app_id.clone(),
                    PID,
                    SessionSource::ExecutableDiscovery,
                ))
                .unwrap();
            // The scheduler checked empty, then a producer appended A's older fact
            // during the scan. The scan now sees the reused PID belonging to B.
            assert!(!fixture.agent.hub.queue.has_queued_events());
            fixture
                .agent
                .hub
                .queue
                .push_observed(QueuedEvent::Filesystem(old), old_proof);
            fixture.process(1, generation);
            let (snapshot, proof) = fixture.event(1);
            fixture.recover(&snapshot, &proof);
            assert_eq!(
                fixture
                    .agent
                    .db
                    .session_for_pid(PID)
                    .unwrap()
                    .unwrap()
                    .app_id,
                fixture.apps[1].app_id
            );
            fixture.agent.process_events().unwrap();
            fixture.no_foreign(1, 0);
            assert_eq!(
                fixture
                    .agent
                    .db
                    .list_file_states(&fixture.apps[0].app_id, None)
                    .unwrap()[0]
                    .trust,
                FileStateTrust::VerifyRequired
            );
            assert!(fixture.agent.db.known_roots(None).unwrap().is_empty());
            fixture.finish();
        }
    }
}

#[test]
fn stale_appended_exec_is_refused_and_a_genuine_fresh_generation_recovers() {
    for (beta_generation, gamma_generation) in [(1, 1), (2, 3)] {
        let fixture = Fixture::new("collector-stale-exec-proof");
        let (initial, initial_proof) = fixture.event(0);
        fixture.recover(&initial, &initial_proof);
        fixture.process(1, beta_generation);
        let (stale, stale_proof) = fixture.event(1);
        fixture
            .agent
            .hub
            .queue
            .push_observed(QueuedEvent::Process(stale), Some(stale_proof));
        fixture.process(2, gamma_generation);
        let write = fs_event(FsEventKind::Write, PID, &fixture.saves[2]);
        let proof = fixture.fs_proof(write.at);
        fixture
            .agent
            .hub
            .queue
            .push_observed(QueuedEvent::Filesystem(write), Some(proof));
        fixture.agent.process_events().unwrap();
        fixture.no_foreign(0, 2);
        fixture.no_foreign(1, 2);

        let (fresh, proof) = fixture.event(2);
        fixture
            .agent
            .hub
            .queue
            .push_observed(QueuedEvent::Process(fresh), Some(proof));
        let write = fs_event(FsEventKind::Write, PID, &fixture.saves[2]);
        let proof = fixture.fs_proof(write.at);
        fixture
            .agent
            .hub
            .queue
            .push_observed(QueuedEvent::Filesystem(write), Some(proof));
        fixture.agent.process_events().unwrap();
        assert_eq!(
            fixture
                .agent
                .db
                .session_for_pid(PID)
                .unwrap()
                .unwrap()
                .app_id,
            fixture.apps[2].app_id
        );
        assert!(fixture
            .agent
            .db
            .pending_app_mutations(&fixture.apps[2].app_id, 100)
            .unwrap()
            .iter()
            .any(|mutation| Path::new(&mutation.path) == fixture.saves[2]));
        fixture.no_foreign(0, 2);
        fixture.no_foreign(1, 2);
        fixture.finish();
    }
}

#[test]
fn failed_ingestion_restores_original_source_proof_and_fifo_tail() {
    let fixture = Fixture::new("collector-proof-error-tail");
    let (initial, initial_proof) = fixture.event(0);
    fixture.recover(&initial, &initial_proof);
    let registry = fixture
        .agent
        .config
        .home
        .join(".local/share/noland-runtime/apps");
    std::fs::create_dir_all(&registry).unwrap();
    let invalid = registry.join("malformed.json");
    std::fs::write(&invalid, b"{invalid").unwrap();
    let write = fs_event(FsEventKind::Write, PID, &fixture.saves[0]);
    let proof = fixture.fs_proof(write.at);
    fixture
        .agent
        .hub
        .queue
        .push_observed(QueuedEvent::Filesystem(write), Some(proof.clone()));
    fixture.agent.hub.report_filesystem_gap(PID);
    assert!(fixture.agent.process_events().is_err());
    let (prefix, _) = fixture.agent.hub.queue.drain_observed_prefix(1);
    assert!(matches!(prefix[0].event, QueuedEvent::Filesystem(_)));
    assert!(prefix[0]
        .process_proof
        .as_ref()
        .unwrap()
        .same_process(&proof));
    fixture.agent.hub.queue.restore_observed_front(prefix);
    assert!(fixture.agent.hub.queue.loss_snapshot().is_some());
    std::fs::remove_file(invalid).unwrap();
    fixture.agent.process_events().unwrap();
    assert!(!fixture.agent.hub.queue.has_queued_events());
    assert!(fixture.agent.hub.queue.loss_snapshot().is_none());
    fixture.finish();
}
