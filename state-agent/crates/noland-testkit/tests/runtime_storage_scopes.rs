//! End-to-end scope changes through the actual capture, encrypted storage and
//! streaming restore libraries, with no live runtime or provider credentials.
use chrono::Utc;
use noland_crypto::MasterKey;
use noland_restore::{prepare_restore, restore_pipelined, PipelineOptions};
use noland_state_agent::{backup::run_backup, AgentConfig, StateAgent};
use noland_state_core::*;
use noland_storage::{read_pack_index, LocalStorage};
use noland_testkit::Harness;
use serde_json::json;
use uuid::Uuid;

#[cfg(unix)]
#[tokio::test]
async fn complete_capture_restores_symlinks_without_reading_or_including_their_targets() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let harness = Harness::new();
    let mut config = AgentConfig::isolated(harness.root.clone());
    config.home = harness.home.clone();
    let agent = StateAgent::boot(config).unwrap();
    let app = AppId::desktop("symlink-scope");
    let install = harness.home.join("Games/LinkFixture");
    std::fs::create_dir_all(install.join("romfs")).unwrap();
    std::fs::write(install.join("romfs/data.bin"), b"owned asset").unwrap();
    let external_dir = harness.root.join("external");
    std::fs::create_dir_all(&external_dir).unwrap();
    let external = external_dir.join("unreadable.bin");
    std::fs::write(&external, b"must not be captured").unwrap();
    std::fs::set_permissions(&external, std::fs::Permissions::from_mode(0o000)).unwrap();
    for (name, target) in [
        ("file-link", std::path::PathBuf::from("romfs/data.bin")),
        ("directory-link", std::path::PathBuf::from("romfs")),
        ("relative-link", std::path::PathBuf::from("file-link")),
        ("external-link", external.clone()),
        ("external-directory-link", external_dir.clone()),
        ("dangling-link", std::path::PathBuf::from("not-created")),
    ] {
        symlink(target, install.join(name)).unwrap();
    }
    agent
        .db
        .upsert_app(&AppIdentity::new(app.clone(), "Link Fixture"))
        .unwrap();
    agent
        .db
        .add_known_root(&app, "install", install.to_str().unwrap())
        .unwrap();
    let key = MasterKey::generate();
    let storage = LocalStorage::new(harness.root.join("cloud"));
    let first = capture(
        &agent,
        &app,
        &storage,
        &key,
        BackupMode::CompleteApplication,
        "include",
    )
    .await;
    assert_eq!(first.files.len(), 7);
    assert_eq!(
        first
            .files
            .iter()
            .filter(|file| file.file_type == "symlink")
            .count(),
        6
    );
    assert!(first
        .files
        .iter()
        .filter(|file| file.file_type == "symlink")
        .all(|file| {
            let target = file.symlink_target.as_ref().unwrap();
            file.chunks.is_empty()
                && file.size == target.len() as u64
                && file.file_hash == noland_cas::blake3_hex(target.as_bytes())
        }));
    let unchanged = capture(
        &agent,
        &app,
        &storage,
        &key,
        BackupMode::CompleteApplication,
        "include",
    )
    .await;
    assert_eq!(unchanged.bundle_id, first.bundle_id);
    std::fs::remove_file(install.join("relative-link")).unwrap();
    symlink("romfs/data.bin", install.join("relative-link")).unwrap();
    let changed = capture(
        &agent,
        &app,
        &storage,
        &key,
        BackupMode::CompleteApplication,
        "include",
    )
    .await;
    assert_ne!(
        changed.bundle_id, first.bundle_id,
        "link changes cannot become a count-only no-op"
    );
    let index = read_pack_index(&storage, &key, &app, changed.bundle_id)
        .await
        .unwrap();
    assert_eq!(
        index.len(),
        1,
        "only the owned regular file has chunk payloads"
    );
    assert_eq!(index[0].chunk_hash, noland_cas::blake3_hex(b"owned asset"));
    let restored_home = harness.root.join("restored-home");
    let roots = LogicalRootMap::from_home(&restored_home);
    let plan = prepare_restore(
        &storage,
        &key,
        &agent.config.paths,
        &app,
        changed.bundle_id,
        RestoreMode::CompleteApplication,
    )
    .await
    .unwrap();
    restore_pipelined(
        &storage,
        &key,
        &plan,
        &index,
        &roots,
        None,
        None,
        PipelineOptions::default(),
        |_| Ok(()),
    )
    .await
    .unwrap();
    for file in &changed.files {
        let destination = roots
            .resolve(&file.logical_root_parsed().unwrap())
            .unwrap()
            .join(&file.relative_path);
        if file.file_type == "symlink" {
            assert!(std::fs::symlink_metadata(&destination)
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(
                std::fs::read_link(destination).unwrap(),
                std::path::PathBuf::from(file.symlink_target.as_ref().unwrap())
            );
        } else {
            assert_eq!(std::fs::read(destination).unwrap(), b"owned asset");
        }
    }
    assert_eq!(
        std::fs::metadata(external).unwrap().permissions().mode() & 0o777,
        0
    );

    // Capture must reject the same unsupported link text as restore, before
    // advancing the committed revision or ever opening a target.
    for target in ["/proc/self/mem", "/sys/kernel", "/dev/null", "../outside"] {
        std::fs::remove_file(install.join("relative-link")).unwrap();
        symlink(target, install.join("relative-link")).unwrap();
        let result = run_backup(
            &agent,
            &app,
            BackupMode::CompleteApplication,
            BackupPerformanceMode::Balanced,
            &storage,
            &key,
            None,
        )
        .await;
        assert!(
            matches!(&result, Err(StateError::UnsafePath(_))),
            "target {target}: {result:?}"
        );
        assert_eq!(
            agent.db.latest_commit(&app).unwrap().unwrap().1,
            changed.bundle_id
        );
    }
}

async fn capture(
    agent: &StateAgent,
    app: &AppId,
    storage: &LocalStorage,
    key: &MasterKey,
    mode: BackupMode,
    caches: &str,
) -> BundleManifest {
    let id = Uuid::new_v4();
    agent
        .db
        .upsert_operation(&OperationRecord {
            operation_id: id,
            kind: "backup".into(),
            app_id: Some(app.clone()),
            state: BackupState::Queued.as_str().into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            last_error: None,
            detail_json: json!({"mode": mode.as_str(), "regenerable_caches": caches}),
        })
        .unwrap();
    run_backup(
        agent,
        app,
        mode,
        BackupPerformanceMode::Balanced,
        storage,
        key,
        Some(id),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn runtime_assets_saves_and_optional_caches_keep_separate_scopes() {
    let harness = Harness::new();
    let mut config = AgentConfig::isolated(harness.root.clone());
    config.home = harness.home.clone();
    let agent = StateAgent::boot(config).unwrap();
    let app = AppId::desktop("runtime-scope");
    let install = harness
        .home
        .join(".local/share/noland-runtime/app-data/fixture/Games/Hookshot");
    let fixture: &[(&str, &[u8])] = &[
        ("hookshot", b"executable-fixture"),
        (
            "romfs/SecretRoom.dat",
            b"ordinary texture with secret in its filename",
        ),
        ("romfs/assets.bin", b"required immutable assets"),
        ("user/save/slot.sav", b"precious save"),
        ("user/config/options.ini", b"volume=42"),
        ("hookshot_programs.bin", b"regenerable programs"),
        ("mkw_vk_pipelines_play_12.bin", b"regenerable pipelines"),
    ];
    for (relative, bytes) in fixture {
        let path = install.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    let mut identity = AppIdentity::new(app.clone(), "Hookshot");
    identity.canonical_executable = Some(install.join("hookshot"));
    agent.db.upsert_app(&identity).unwrap();
    agent
        .db
        .add_known_root(&app, "install", install.to_str().unwrap())
        .unwrap();
    let key = MasterKey::generate();
    let storage = LocalStorage::new(harness.root.join("cloud"));

    let complete = capture(
        &agent,
        &app,
        &storage,
        &key,
        BackupMode::CompleteApplication,
        "include",
    )
    .await;
    assert_eq!(complete.files.len(), fixture.len());
    assert_eq!(
        complete
            .files
            .iter()
            .filter(|file| file.semantic_role == SemanticRole::UserState)
            .count(),
        2
    );
    assert!(complete
        .files
        .iter()
        .find(|file| file.relative_path.ends_with("romfs/SecretRoom.dat"))
        .is_some_and(|file| file.semantic_role == SemanticRole::AppContent));

    // Changing mode must not return an unchanged parent with a broader scope.
    let personal = capture(
        &agent,
        &app,
        &storage,
        &key,
        BackupMode::PersonalState,
        "include",
    )
    .await;
    assert_eq!(personal.mode, BackupMode::PersonalState);
    assert_eq!(personal.files.len(), 2);
    assert!(personal
        .files
        .iter()
        .all(|file| file.relative_path.contains("/user/")));

    let lean = capture(
        &agent,
        &app,
        &storage,
        &key,
        BackupMode::CompleteApplication,
        "exclude",
    )
    .await;
    assert_eq!(lean.files.len(), fixture.len() - 2);
    assert_eq!(lean.environment["regenerable_caches"], "exclude");
    assert!(!lean
        .tombstones
        .iter()
        .any(|file| is_regenerable_game_cache(std::path::Path::new(&file.relative_path))));
    for (relative, bytes) in fixture {
        assert_eq!(
            std::fs::read(install.join(relative)).unwrap(),
            *bytes,
            "capture policy must not remove or mutate source data"
        );
    }

    let restored_home = harness.root.join("fresh-home");
    let roots = LogicalRootMap::from_home(&restored_home);
    let plan = prepare_restore(
        &storage,
        &key,
        &agent.config.paths,
        &app,
        lean.bundle_id,
        RestoreMode::CompleteApplication,
    )
    .await
    .unwrap();
    let index = read_pack_index(&storage, &key, &app, lean.bundle_id)
        .await
        .unwrap();
    let report = restore_pipelined(
        &storage,
        &key,
        &plan,
        &index,
        &roots,
        None,
        None,
        PipelineOptions::default(),
        |_| Ok(()),
    )
    .await
    .unwrap();
    assert_eq!(report.restore.published_entries, lean.files.len());
    for file in &lean.files {
        let path = roots
            .resolve(&file.logical_root_parsed().unwrap())
            .unwrap()
            .join(&file.relative_path);
        assert_eq!(noland_cas::blake3_file(&path).unwrap(), file.file_hash);
    }

    // Re-enabling caches is a new capture scope, even if no filesystem event occurred.
    let warm = capture(
        &agent,
        &app,
        &storage,
        &key,
        BackupMode::CompleteApplication,
        "include",
    )
    .await;
    assert_eq!(warm.files.len(), fixture.len());
    assert_ne!(warm.bundle_id, lean.bundle_id);
    assert_eq!(warm.environment["regenerable_caches"], "include");
}
