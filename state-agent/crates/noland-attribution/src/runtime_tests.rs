use super::*;
use std::fs;
use std::os::unix::fs::symlink;

struct RuntimeFixture {
    root: PathBuf,
    home: PathBuf,
    proc: PathBuf,
    private: [PathBuf; 2],
    apps: [AppIdentity; 2],
}

impl RuntimeFixture {
    fn new() -> Self {
        let root = std::env::current_dir()
            .unwrap()
            .join("target/runtime-attribution-tests")
            .join(Uuid::new_v4().to_string());
        fs::create_dir_all(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let home = root.join("home");
        let proc = root.join("proc");
        fs::create_dir_all(proc.join("self")).unwrap();
        fs::write(
            proc.join("self/mountinfo"),
            "1 0 8:1 / / rw - ext4 /dev/root rw\n",
        )
        .unwrap();
        let private =
            ["one", "two"].map(|id| home.join(".local/share/noland-runtime/app-data").join(id));
        let apps = [0, 1].map(|index| {
            let game = private[index].join("Games/Identical");
            fs::create_dir_all(game.join("saves/old")).unwrap();
            fs::create_dir_all(game.join("saves/new")).unwrap();
            fs::write(game.join("game"), b"same executable").unwrap();
            fs::write(game.join("saves/new/current.sav"), b"save").unwrap();
            AppIdentity {
                canonical_executable: Some(game.join("game")),
                launcher: Some(LauncherKind::Native),
                ..AppIdentity::new(AppId::desktop(&format!("runtime-{index}")), "Identical")
            }
        });
        let fixture = Self {
            root,
            home,
            proc,
            private,
            apps,
        };
        fixture.process(41, 0, 1);
        fixture.process(42, 1, 1);
        fixture
    }

    fn process(&self, pid: i32, index: usize, start: u64) {
        let process = self.proc.join(pid.to_string());
        let _ = fs::remove_dir_all(&process);
        fs::create_dir_all(process.join("ns")).unwrap();
        fs::create_dir_all(process.join("root/home")).unwrap();
        symlink(&self.private[index], process.join("root/home/runtime")).unwrap();
        symlink(format!("mnt:[{}]", 1000 + index), process.join("ns/mnt")).unwrap();
        let mut fields = vec!["0".to_string(); 20];
        fields[0] = "S".into();
        fields[1] = "1".into();
        fields[19] = start.to_string();
        fs::write(
            process.join("stat"),
            format!("{pid} (game) {}", fields.join(" ")),
        )
        .unwrap();
        fs::write(process.join("mountinfo"), format!(
            "10 0 0:10 / / rw - tmpfs tmpfs rw\n11 10 8:1 {} /home/runtime rw - ext4 /dev/root rw\n",
            self.private[index].display()
        )).unwrap();
    }

    fn register(&self, db: &StateDb, sessions: bool) {
        for (index, app) in self.apps.iter().enumerate() {
            db.upsert_app(app).unwrap();
            db.add_known_root(
                &app.app_id,
                "install",
                &self.private[index]
                    .join("Games/Identical")
                    .to_string_lossy(),
            )
            .unwrap();
            if sessions {
                db.insert_session(&AppSession::new(
                    app.app_id.clone(),
                    41 + index as i32,
                    SessionSource::ExecutableDiscovery,
                ))
                .unwrap();
            }
        }
    }

    fn engine<'a>(&self, db: &'a StateDb) -> AttributionEngine<'a> {
        let mut engine = AttributionEngine::new(
            db,
            LogicalRootMap::from_home(&self.home),
            AgentPaths::from_roots(self.root.join("state"), self.root.join("run")),
        );
        engine.host_paths = HostPathResolver::with_proc_root(self.proc.clone());
        engine
    }

    fn host(&self, index: usize, suffix: &str) -> PathBuf {
        self.private[index].join("Games/Identical").join(suffix)
    }
}

impl Drop for RuntimeFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn registered_workspace() -> RuntimeFixture {
    use std::os::unix::fs::PermissionsExt;
    let mut fixture = RuntimeFixture::new();
    let source = fixture.home.join("Games/Original/game");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    let mut executable = vec![0; 20];
    executable[..4].copy_from_slice(b"\x7fELF");
    executable[4] = 2;
    executable[5] = 1;
    executable[18] = 62;
    executable.extend_from_slice(b"original immutable executable");
    fs::write(&source, &executable).unwrap();
    fs::set_permissions(&source, fs::Permissions::from_mode(0o700)).unwrap();
    fixture.apps[0].canonical_executable = Some(source.clone());
    let base = fixture.home.join(".local/share/noland-runtime");
    let view = base.join("workspaces/one/view");
    fs::create_dir_all(view.join("user/save")).unwrap();
    fs::write(view.join("game"), b"private updated executable").unwrap();
    fs::set_permissions(view.join("game"), fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(view.join("user/save/progress.sav"), b"private save").unwrap();
    fs::create_dir_all(fixture.private[0].join(".config")).unwrap();
    fs::write(
        fixture.private[0].join(".config/settings.ini"),
        b"volume=42",
    )
    .unwrap();
    fs::create_dir_all(base.join("apps")).unwrap();
    fs::write(
        base.join("apps/one.json"),
        serde_json::to_vec(&serde_json::json!({
            "id": "one", "file": source, "profile": "ubuntu24-x86_64",
            "appimage": false, "writableApp": true, "writablePaths": []
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        base.join("workspaces/one/manifest.json"),
        serde_json::to_vec(&serde_json::json!({
            "schema": 1, "appId": "one", "originalFile": source,
            "appimage": false, "source": source.parent().unwrap(), "executable": "game",
            "importSnapshotSha256": "a".repeat(64), "legacyPaths": [],
            "stats": {"files": 2, "bytes": 40, "reflinkFiles": 0}
        }))
        .unwrap(),
    )
    .unwrap();
    symlink(&view, fixture.proc.join("41/root/app")).unwrap();
    let mountinfo = fixture.proc.join("41/mountinfo");
    let mut mounts = fs::read_to_string(&mountinfo).unwrap();
    mounts.push_str(&format!(
        "12 10 8:1 {} /app rw - ext4 /dev/root rw\n",
        view.to_string_lossy().replace(' ', "\\040")
    ));
    fs::write(mountinfo, mounts).unwrap();
    fixture
}

#[test]
fn verified_workspace_exec_and_writes_use_original_exact_app_id_not_basename() {
    let fixture = registered_workspace();
    let db = StateDb::open_in_memory().unwrap();
    fixture.register(&db, false);
    let mut engine = fixture.engine(&db);
    let session = engine
        .ingest_process(&noland_observer::process_exec(41, 1, "/app/game"))
        .unwrap()
        .unwrap();
    assert_eq!(session.app_id, fixture.apps[0].app_id);
    assert_eq!(
        db.get_app(&session.app_id)
            .unwrap()
            .unwrap()
            .canonical_executable,
        fixture.apps[0].canonical_executable,
        "never point the indexed executable at a legacy placeholder"
    );
    for path in [
        "/app/user/save/progress.sav",
        "/home/runtime/.config/settings.ini",
    ] {
        let association = engine
            .ingest_fs(&noland_observer::fs_event(FsEventKind::Write, 41, path))
            .unwrap()
            .unwrap();
        assert_eq!(association.app_id, fixture.apps[0].app_id);
    }
    let view = fixture
        .home
        .join(".local/share/noland-runtime/workspaces/one/view");
    assert!(db
        .get_path_by_canonical(view.join("user/save/progress.sav").to_str().unwrap())
        .unwrap()
        .is_some());
    assert!(db
        .get_path_by_canonical("/app/user/save/progress.sav")
        .unwrap()
        .is_none());
    assert!(db
        .get_path_by_canonical(
            fixture.private[0]
                .join(".config/settings.ini")
                .to_str()
                .unwrap()
        )
        .unwrap()
        .is_some());
    assert!(
        fs::read(fixture.apps[0].canonical_executable.as_ref().unwrap())
            .unwrap()
            .ends_with(b"original immutable executable")
    );
}

#[test]
fn host_workspace_executable_alias_uses_the_same_verified_binding() {
    let fixture = registered_workspace();
    let db = StateDb::open_in_memory().unwrap();
    fixture.register(&db, false);
    let mut engine = fixture.engine(&db);
    let view = fixture
        .home
        .join(".local/share/noland-runtime/workspaces/one/view");
    let session = engine
        .ingest_process(&noland_observer::process_exec(41, 1, view.join("game")))
        .unwrap()
        .unwrap();
    assert_eq!(session.app_id, fixture.apps[0].app_id);
    assert!(
        engine
            .ingest_process(&noland_observer::process_exec(
                41,
                1,
                view.join("user/save/progress.sav")
            ))
            .unwrap()
            .is_none(),
        "a file in the view is not necessarily its executable"
    );
}

#[test]
fn foreign_session_cannot_borrow_registered_view_even_through_a_host_alias() {
    let fixture = registered_workspace();
    let db = StateDb::open_in_memory().unwrap();
    fixture.register(&db, false);
    db.insert_session(&AppSession::new(
        fixture.apps[1].app_id.clone(),
        41,
        SessionSource::ExecutableDiscovery,
    ))
    .unwrap();
    let mut engine = fixture.engine(&db);
    let path = fixture
        .home
        .join(".local/share/noland-runtime/workspaces/one/view/user/save/progress.sav");
    assert!(engine
        .ingest_fs(&noland_observer::fs_event(FsEventKind::Write, 41, &path))
        .unwrap()
        .is_none());
    assert!(db
        .get_path_by_canonical(path.to_str().unwrap())
        .unwrap()
        .is_none());
    assert!(engine
        .ingest_fs(&noland_observer::fs_event(
            FsEventKind::Write,
            41,
            "/app/user/save/progress.sav"
        ))
        .unwrap()
        .is_none());
}

#[test]
fn forged_runtime_registration_is_not_process_or_file_authority() {
    let fixture = registered_workspace();
    let db = StateDb::open_in_memory().unwrap();
    fixture.register(&db, false);
    let record = fixture
        .home
        .join(".local/share/noland-runtime/apps/one.json");
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
    value["file"] = serde_json::json!(fixture.apps[1].canonical_executable);
    fs::write(record, serde_json::to_vec(&value).unwrap()).unwrap();
    let mut engine = fixture.engine(&db);
    assert!(engine
        .ingest_process(&noland_observer::process_exec(41, 1, "/app/game"))
        .is_err());
    assert!(db.session_for_pid(41).unwrap().is_none());
}

#[test]
fn runtime_mutations_persist_only_host_paths_and_dirty_both_rename_parents() {
    let fixture = RuntimeFixture::new();
    let db = StateDb::open_in_memory().unwrap();
    fixture.register(&db, true);
    let mut engine = fixture.engine(&db);
    for (pid, index) in [(41, 0), (42, 1)] {
        let association = engine
            .ingest_fs(&noland_observer::fs_event(
                FsEventKind::Write,
                pid,
                "/home/runtime/Games/Identical/saves/new/current.sav",
            ))
            .unwrap()
            .unwrap();
        assert_eq!(association.app_id, fixture.apps[index].app_id);
    }
    let rename = EbpfFilesystemFact {
        kind: FsEventKind::Rename,
        tgid: 41,
        path: "/home/runtime/Games/Identical/saves/old/removed.sav".into(),
        second_path: Some("/home/runtime/Games/Identical/saves/new/current.sav".into()),
        at: Utc::now(),
        ..EbpfFilesystemFact::default()
    };
    engine.ingest_ebpf_fs(&rename).unwrap().unwrap();
    for kind in [FsEventKind::Unlink, FsEventKind::Rmdir, FsEventKind::Mkdir] {
        engine
            .ingest_fs(&noland_observer::fs_event(
                kind,
                41,
                format!("/home/runtime/Games/Identical/saves/gone-{kind:?}"),
            ))
            .unwrap()
            .unwrap();
    }
    let app = &fixture.apps[0].app_id;
    let mutations = db.pending_app_mutations(app, 100).unwrap();
    let rename = mutations
        .iter()
        .find(|mutation| mutation.kind == AppMutationKind::Rename)
        .unwrap();
    assert_eq!(
        Path::new(&rename.path),
        fixture.host(0, "saves/new/current.sav")
    );
    assert_eq!(
        rename.previous_path.as_deref(),
        Some(fixture.host(0, "saves/old/removed.sav").to_str().unwrap())
    );
    for (path, _) in db.associations_for_app(app).unwrap() {
        assert!(Path::new(&path.canonical_path).starts_with(&fixture.private[0]));
    }
    assert!(db
        .get_path_by_canonical("/home/runtime/Games/Identical/saves/new/current.sav")
        .unwrap()
        .is_none());
    let dirty = db.list_dirty_roots(Some(app)).unwrap();
    for suffix in ["saves/old", "saves/new"] {
        assert!(dirty.iter().any(|root| Path::new(&root.canonical_root)
            == fixture.host(0, suffix)
            && root.requires_reconciliation));
    }
    assert!(db
        .list_dirty_apps()
        .unwrap()
        .iter()
        .any(|dirty| dirty.app_id == *app && dirty.requires_reconciliation));
}

#[test]
fn wrong_session_and_unavailable_mapping_require_reconciliation_without_indexing() {
    let fixture = RuntimeFixture::new();
    let db = StateDb::open_in_memory().unwrap();
    fixture.register(&db, false);
    // A stale PID binding or basename collision must not borrow app two's ID.
    db.insert_session(&AppSession::new(
        fixture.apps[1].app_id.clone(),
        41,
        SessionSource::ExecutableDiscovery,
    ))
    .unwrap();
    let mut engine = fixture.engine(&db);
    let event = noland_observer::fs_event(
        FsEventKind::Write,
        41,
        "/home/runtime/Games/Identical/saves/new/current.sav",
    );
    assert!(engine.ingest_fs(&event).unwrap().is_none());
    assert!(db
        .get_path_by_canonical(&fixture.host(0, "saves/new/current.sav").to_string_lossy())
        .unwrap()
        .is_none());
    for app in &fixture.apps {
        assert!(db
            .list_dirty_apps()
            .unwrap()
            .iter()
            .any(|dirty| dirty.app_id == app.app_id && dirty.requires_reconciliation));
        assert!(!db.list_dirty_roots(Some(&app.app_id)).unwrap().is_empty());
    }
    fs::remove_file(fixture.proc.join("41/mountinfo")).unwrap();
    assert!(engine.ingest_fs(&event).unwrap().is_none());
    assert!(db
        .get_path_by_canonical(&event.path.to_string_lossy())
        .unwrap()
        .is_none());
}

#[test]
fn runtime_processes_with_identical_executable_names_use_exact_host_owner() {
    let fixture = RuntimeFixture::new();
    let db = StateDb::open_in_memory().unwrap();
    fixture.register(&db, false);
    let mut engine = fixture.engine(&db);
    for (pid, index) in [(42, 1), (41, 0)] {
        let event = noland_observer::process_exec(pid, 1, "/home/runtime/Games/Identical/game");
        let session = engine.ingest_process(&event).unwrap().unwrap();
        assert_eq!(session.app_id, fixture.apps[index].app_id);
        assert_eq!(
            db.get_app(&session.app_id)
                .unwrap()
                .unwrap()
                .canonical_executable,
            fixture.apps[index].canonical_executable
        );
    }
}

#[test]
fn an_unverified_runtime_session_cannot_claim_a_private_home_by_name() {
    let fixture = RuntimeFixture::new();
    let db = StateDb::open_in_memory().unwrap();
    fixture.register(&db, false);
    let app = AppIdentity {
        canonical_executable: Some("/app/game".into()),
        ..AppIdentity::new(AppId::desktop("unverified-runtime"), "Identical")
    };
    db.upsert_app(&app).unwrap();
    db.insert_session(&AppSession::new(
        app.app_id.clone(),
        41,
        SessionSource::ExecutableDiscovery,
    ))
    .unwrap();
    let mut engine = fixture.engine(&db);
    assert!(engine
        .ingest_fs(&noland_observer::fs_event(
            FsEventKind::Write,
            41,
            "/home/runtime/Games/Identical/saves/new/current.sav"
        ))
        .unwrap()
        .is_none());
    assert!(db.associations_for_app(&app.app_id).unwrap().is_empty());
    assert!(db
        .list_dirty_apps()
        .unwrap()
        .iter()
        .any(|dirty| dirty.app_id == fixture.apps[0].app_id && dirty.requires_reconciliation));
}

#[test]
fn ebpf_filesystem_resolution_uses_the_originating_thread_namespace() {
    let fixture = RuntimeFixture::new();
    let db = StateDb::open_in_memory().unwrap();
    fixture.register(&db, true);
    fixture.process(42, 0, 1); // leader namespace differs from its worker
    fixture.process(43, 1, 1);
    let mut engine = fixture.engine(&db);
    let fact = EbpfFilesystemFact {
        kind: FsEventKind::Write,
        tgid: 42,
        tid: 43,
        path: "/home/runtime/Games/Identical/saves/new/current.sav".into(),
        at: Utc::now(),
        ..EbpfFilesystemFact::default()
    };
    let association = engine.ingest_ebpf_fs(&fact).unwrap().unwrap();
    assert_eq!(association.app_id, fixture.apps[1].app_id);
    assert!(db
        .get_path_by_canonical(&fixture.host(1, "saves/new/current.sav").to_string_lossy())
        .unwrap()
        .is_some());
    assert!(db
        .get_path_by_canonical(&fixture.host(0, "saves/new/current.sav").to_string_lossy())
        .unwrap()
        .is_none());
}

#[test]
fn exact_legacy_executable_repair_preserves_id_and_historical_paths() {
    let fixture = RuntimeFixture::new();
    fs::remove_file(fixture.host(1, "game")).unwrap();
    let db = StateDb::open_in_memory().unwrap();
    let original = Path::new("/home/runtime/Games/Identical/game");
    let legacy = fallback_exe_identity(original);
    db.upsert_app(&legacy).unwrap();
    let historical = "/home/runtime/Games/Identical/saves/history.sav";
    let historical_id = db.upsert_path(historical).unwrap();
    let mut engine = fixture.engine(&db);
    let session = engine
        .ingest_process(&noland_observer::process_exec(41, 1, original))
        .unwrap()
        .unwrap();
    assert_eq!(session.app_id, legacy.app_id);
    assert_eq!(
        db.get_app(&legacy.app_id)
            .unwrap()
            .unwrap()
            .canonical_executable,
        Some(fixture.host(0, "game"))
    );
    assert_eq!(
        db.get_path_by_canonical(historical)
            .unwrap()
            .unwrap()
            .path_id,
        historical_id
    );
}

#[test]
fn pending_runtime_event_is_not_reassigned_after_pid_reuse() {
    let fixture = RuntimeFixture::new();
    let db = StateDb::open_in_memory().unwrap();
    fixture.register(&db, false);
    let mut engine = fixture.engine(&db);
    assert!(engine
        .ingest_fs(&noland_observer::fs_event(
            FsEventKind::Write,
            41,
            "/home/runtime/Games/Identical/saves/new/current.sav"
        ))
        .unwrap()
        .is_none());
    assert_eq!(engine.unresolved_len(), 1);
    fixture.process(41, 1, 2);
    db.insert_session(&AppSession::new(
        fixture.apps[1].app_id.clone(),
        41,
        SessionSource::ExecutableDiscovery,
    ))
    .unwrap();
    engine.retry_unresolved().unwrap();
    assert_eq!(engine.unresolved_len(), 0);
    assert!(db
        .associations_for_app(&fixture.apps[1].app_id)
        .unwrap()
        .is_empty());
    assert!(db
        .list_dirty_apps()
        .unwrap()
        .iter()
        .any(|dirty| dirty.app_id == fixture.apps[0].app_id && dirty.requires_reconciliation));
}

#[test]
fn queued_gap_marks_the_affected_runtime_root() {
    let fixture = RuntimeFixture::new();
    let db = StateDb::open_in_memory().unwrap();
    fixture.register(&db, true);
    let hub = ObserverHub::new(noland_state_core::metrics::Metrics::shared());
    hub.report_filesystem_gap(42);
    let mut engine = fixture.engine(&db);
    assert_eq!(process_hub_events(&mut engine, &hub).unwrap(), 1);
    assert!(db
        .list_dirty_apps()
        .unwrap()
        .iter()
        .any(|dirty| dirty.app_id == fixture.apps[1].app_id && dirty.requires_reconciliation));
    assert!(db
        .list_dirty_roots(Some(&fixture.apps[1].app_id))
        .unwrap()
        .iter()
        .all(
            |root| !is_runtime_namespace_path(Path::new(&root.canonical_root))
                && root.requires_reconciliation
        ));
}
