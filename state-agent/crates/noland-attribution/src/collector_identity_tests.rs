//! Independent collector-identity regressions, wired by the parent as a
//! `#[cfg(all(test, unix))]` child module. All files and procfs proofs are synthetic.
//! Ordinary native-app paths deliberately avoid the separate runtime-HOME/view
//! guards, and saves have existing associations but no covering known root.
//! The retained hash is a synthetic cache value, not a content-hashing assertion.

use std::fs::{self, File, FileTimes};
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use noland_observer::{fs_event, process_exec, HostPathResolver, ObserverHub, QueuedEvent};
use noland_state_core::metrics::Metrics;
use noland_state_core::*;
use noland_state_db::StateDb;
use uuid::Uuid;

use crate::{process_hub_events_prefix, AttributionEngine};

const PID: i32 = 41;
const RETAINED_HASH: &str = "synthetic-retained-content-hash";

struct Fixture {
    root: PathBuf,
    home: PathBuf,
    proc_root: PathBuf,
    db: StateDb,
    apps: [AppIdentity; 2],
    saves: [PathBuf; 2],
}

impl Fixture {
    fn new() -> Self {
        // Match attribution's existing fixtures: /tmp is deliberately excluded
        // by production path policy, so put synthetic homes under the checkout.
        let root = std::env::current_dir()
            .expect("test working directory must exist")
            .join("target/collector-identity-tests")
            .join(Uuid::new_v4().to_string());
        fs::create_dir_all(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let home = root.join("home");
        let proc_root = root.join("proc");
        let db = StateDb::open_in_memory().unwrap();
        let apps = ["Alpha", "Beta"].map(|name| {
            let executable = home.join("Games").join(name).join("game");
            fs::create_dir_all(executable.parent().unwrap()).unwrap();
            fs::write(&executable, b"synthetic native executable; never launched").unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
            let app = AppIdentity {
                canonical_executable: Some(executable.clone()),
                launcher: Some(LauncherKind::Native),
                ..AppIdentity::new(
                    AppId::desktop(&format!("collector-{}", name.to_ascii_lowercase())),
                    format!("{name} Native Fixture"),
                )
            };
            db.upsert_app(&app).unwrap();
            db.add_known_root(
                &app.app_id,
                "install",
                executable.parent().unwrap().to_str().unwrap(),
            )
            .unwrap();
            app
        });
        let saves = ["Alpha", "Beta"].map(|name| {
            let path = home.join(".config").join(name).join("save.dat");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, b"old!").unwrap();
            path
        });
        let fixture = Self {
            root,
            home,
            proc_root,
            db,
            apps,
            saves,
        };
        fixture.set_generation(1);
        for index in 0..2 {
            fixture.seed_trusted_save(index);
            assert_eq!(fixture.state(index).trust, FileStateTrust::Trusted);
            assert!(!fixture
                .known_roots()
                .iter()
                .any(|(_, _, root)| fixture.saves[index].starts_with(Path::new(root))));
        }
        assert!(fixture.db.list_dirty_apps().unwrap().is_empty());
        assert!(fixture.db.session_for_pid(PID).unwrap().is_none());
        fixture.assert_no_foreign_save(0, 1);
        fixture.assert_no_foreign_save(1, 0);
        fixture
    }

    fn set_generation(&self, start_ticks: u64) {
        let process = self.proc_root.join(PID.to_string());
        fs::create_dir_all(process.join("ns")).unwrap();
        fs::create_dir_all(process.join("root")).unwrap();
        let namespace = process.join("ns/mnt");
        if fs::symlink_metadata(&namespace).is_err() {
            symlink("mnt:[1041]", &namespace).unwrap();
        }
        // Fields after comm start at stat field 3; starttime is field 22.
        // process_token needs only stat, ns/mnt and root metadata for these
        // ordinary host paths. No real /proc or mount namespace is consulted.
        let mut fields = vec!["0".to_owned(); 20];
        fields[0] = "S".into();
        fields[1] = "1".into();
        fields[19] = start_ticks.to_string();
        fs::write(
            process.join("stat"),
            format!("{PID} (synthetic game) {}", fields.join(" ")),
        )
        .unwrap();
    }

    fn engine(&self) -> AttributionEngine<'_> {
        let mut engine = AttributionEngine::new(
            &self.db,
            LogicalRootMap::from_home(&self.home),
            AgentPaths::from_roots(self.root.join("agent-state"), self.root.join("agent-run")),
        );
        engine.host_paths = HostPathResolver::with_proc_root(self.proc_root.clone());
        engine
    }

    fn seed_trusted_save(&self, index: usize) {
        let path = &self.saves[index];
        let app = &self.apps[index].app_id;
        let logical = LogicalRootMap::from_home(&self.home)
            .classify(path)
            .unwrap();
        let metadata = fs::symlink_metadata(path).unwrap();
        let mtime_ns = metadata.mtime() * 1_000_000_000 + metadata.mtime_nsec();
        let now = Utc::now();
        let path_id = self.db.upsert_path(path.to_str().unwrap()).unwrap();
        self.db
            .update_path_meta(
                path_id,
                &PathRecord {
                    path_id,
                    canonical_path: path.to_str().unwrap().into(),
                    logical_root: Some(logical.logical_root.as_token()),
                    relative_path: Some(logical.relative_path.clone()),
                    file_type: Some("file".into()),
                    inode: Some(metadata.ino() as i64),
                    mount_id: Some(metadata.dev() as i64),
                    size: Some(metadata.len() as i64),
                    mtime_ns: Some(mtime_ns),
                    mode: Some(metadata.mode() as i64),
                    uid: Some(metadata.uid() as i64),
                    gid: Some(metadata.gid() as i64),
                    content_hash: Some(RETAINED_HASH.into()),
                    last_scanned_at: Some(now.timestamp()),
                },
            )
            .unwrap();
        self.db
            .upsert_association(&PathAssociation {
                app_id: app.clone(),
                path_id,
                confidence: CONF_REPEATED,
                evidence: vec![Evidence::new(EvidenceKind::ReconciliationDelta)],
                persistence_class: PersistenceClass::PersistentState,
                semantic_role: SemanticRole::UserState,
                first_seen_at: now,
                last_seen_at: now,
            })
            .unwrap();
        self.db
            .upsert_file_state(&FileStateRecord {
                app_id: app.clone(),
                logical_root: logical.logical_root.as_token(),
                relative_path: logical.relative_path,
                canonical_path: Some(path.to_str().unwrap().into()),
                file_type: FileType::File,
                size: metadata.len(),
                mtime_ns,
                inode: Some(metadata.ino()),
                mount_id: Some(metadata.dev()),
                mode: Some(metadata.mode()),
                content_hash: Some(RETAINED_HASH.into()),
                trust: FileStateTrust::Trusted,
                last_seen_at: now,
                last_hashed_at: Some(now),
            })
            .unwrap();
    }

    fn state(&self, index: usize) -> FileStateRecord {
        let logical = LogicalRootMap::from_home(&self.home)
            .classify(&self.saves[index])
            .unwrap();
        self.db
            .get_file_state(
                &self.apps[index].app_id,
                &logical.logical_root.as_token(),
                &logical.relative_path,
            )
            .unwrap()
            .unwrap()
    }

    fn rewrite_without_changing_fast_identity(&self, index: usize) {
        let path = &self.saves[index];
        let modified = fs::metadata(path).unwrap().modified().unwrap();
        fs::write(path, b"new!").unwrap();
        File::open(path)
            .unwrap()
            .set_times(FileTimes::new().set_modified(modified))
            .unwrap();
        let metadata = fs::metadata(path).unwrap();
        assert!(self.state(index).fast_identity_matches(
            metadata.len(),
            metadata.mtime() * 1_000_000_000 + metadata.mtime_nsec(),
            Some(metadata.ino()),
            Some(metadata.dev()),
        ));
    }

    fn known_roots(&self) -> Vec<(AppId, String, String)> {
        let mut roots = self.db.known_roots(None).unwrap();
        roots.sort();
        roots
    }

    fn assert_no_foreign_save(&self, app_index: usize, save_index: usize) {
        let path = &self.saves[save_index];
        let record = self
            .db
            .get_path_by_canonical(path.to_str().unwrap())
            .unwrap()
            .unwrap();
        let associations = self.db.associations_for_path(record.path_id).unwrap();
        assert!(
            associations
                .iter()
                .any(|association| association.app_id == self.apps[save_index].app_id),
            "the existing original-owner association must not be erased"
        );
        assert!(
            associations
                .iter()
                .all(|association| association.app_id != self.apps[app_index].app_id),
            "a reused or stale PID must not lend another app authority over {}",
            path.display()
        );
        assert!(!self
            .db
            .pending_app_mutations(&self.apps[app_index].app_id, 100)
            .unwrap()
            .iter()
            .any(|mutation| Path::new(&mutation.path) == path));
    }

    fn assert_fenced(&self, index: usize, previous_generation: i64) {
        let app = &self.apps[index].app_id;
        let state = self.state(index);
        assert_eq!(
            state.trust,
            FileStateTrust::VerifyRequired,
            "original-owner trust must be fenced even outside registered roots: {app}"
        );
        assert_eq!(state.content_hash.as_deref(), Some(RETAINED_HASH));
        assert!(self.db.capture_generation(app).unwrap() > previous_generation);
        assert!(self
            .db
            .list_dirty_apps()
            .unwrap()
            .iter()
            .any(|dirty| dirty.app_id == *app && dirty.requires_reconciliation));
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn pending_pid_generation_mismatch_fences_original_save_outside_known_roots() {
    let fixture = Fixture::new();
    let roots = fixture.known_roots();
    let before = fixture
        .db
        .capture_generation(&fixture.apps[0].app_id)
        .unwrap();
    fixture.rewrite_without_changing_fast_identity(0);
    let mut first = fixture.engine();
    let original_token = first.host_paths.process_token(PID).unwrap();
    assert!(first
        .ingest_fs(&fs_event(FsEventKind::Write, PID, &fixture.saves[0]))
        .unwrap()
        .is_none());
    assert_eq!(first.unresolved_len(), 1);
    let pending = first.take_pending_state();
    drop(first);

    // The old Exit was not observed. B's fresh Exec installs the replacement
    // session before retrying the original fact. Direct public ingestion is
    // intentional: no ObserverHub/global queue-loss recovery can mask A's fence.
    fixture.set_generation(2);
    let mut next = fixture.engine().with_pending_state(pending);
    assert_ne!(next.host_paths.process_token(PID).unwrap(), original_token);
    let session = next
        .ingest_process(&process_exec(
            PID,
            1,
            fixture.apps[1].canonical_executable.as_ref().unwrap(),
        ))
        .unwrap()
        .unwrap();
    assert_eq!(session.app_id, fixture.apps[1].app_id);
    assert_eq!(
        fixture.db.session_for_pid(PID).unwrap().unwrap().app_id,
        fixture.apps[1].app_id
    );
    fixture.assert_no_foreign_save(1, 0);
    fixture.assert_fenced(0, before);
    assert_eq!(fixture.known_roots(), roots);
}

fn assert_loss_does_not_lend_stale_session_authority(ebpf: bool) {
    let fixture = Fixture::new();
    let roots = fixture.known_roots();
    let generations = fixture
        .apps
        .each_ref()
        .map(|app| fixture.db.capture_generation(&app.app_id).unwrap());
    fixture
        .db
        .insert_session(&AppSession::new(
            fixture.apps[0].app_id.clone(),
            PID,
            SessionSource::ExecutableDiscovery,
        ))
        .unwrap();
    let original_token = fixture.engine().host_paths.process_token(PID).unwrap();
    fixture.set_generation(2);
    fixture.rewrite_without_changing_fast_identity(1);
    let mut engine = fixture.engine();
    assert_ne!(
        engine.host_paths.process_token(PID).unwrap(),
        original_token
    );
    assert_eq!(
        fixture.db.session_for_pid(PID).unwrap().unwrap().app_id,
        fixture.apps[0].app_id,
        "the DB intentionally still contains A's stale PID binding"
    );
    let hub = ObserverHub::new(Arc::new(Metrics::default()));
    // Exit(A) and Exec(B) were lost; only B's ordinary host-path write survives.
    hub.queue.report_loss(2);
    let event = if ebpf {
        let metadata = fs::metadata(&fixture.saves[1]).unwrap();
        QueuedEvent::EbpfFilesystem(EbpfFilesystemFact {
            kind: FsEventKind::Write,
            tgid: PID,
            tid: PID,
            path: fixture.saves[1].clone(),
            inode: Some(metadata.ino()),
            device: Some(metadata.dev()),
            io_result: Some(4),
            source: ObservationSource::Ebpf,
            sequence: 3,
            at: Utc::now(),
            ..EbpfFilesystemFact::default()
        })
    } else {
        QueuedEvent::Filesystem(fs_event(FsEventKind::Write, PID, &fixture.saves[1]))
    };
    hub.queue.push(event);
    assert!(hub.queue.loss_snapshot().is_some());

    // Safe refusal or quarantine is permitted. Do not prescribe whether the
    // ambiguous fact remains in the hub or opaque pending state, or require a
    // drained queue. Either outcome must already have durable owner fences.
    let _processing = process_hub_events_prefix(&mut engine, &hub, 1);
    fixture.assert_no_foreign_save(0, 1);
    for (index, generation) in generations.into_iter().enumerate() {
        fixture.assert_fenced(index, generation);
    }
    assert_eq!(fixture.known_roots(), roots);
}

#[test]
fn queue_loss_cannot_lend_stale_native_session_authority_to_a_legacy_write() {
    assert_loss_does_not_lend_stale_session_authority(false);
}

#[test]
fn queue_loss_cannot_lend_stale_native_session_authority_to_an_ebpf_write() {
    assert_loss_does_not_lend_stale_session_authority(true);
}
