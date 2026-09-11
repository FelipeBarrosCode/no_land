use noland_attribution::AttributionEngine;
use noland_crypto::MasterKey;
use noland_observer::{fs_event, process_exec, ObserverHub};
use noland_restore::{download_and_verify, prepare_restore, RestoreTarget, RestoreTransaction};
use noland_state_agent::backup::run_backup_to_local;
use noland_state_agent::{AgentConfig, StateAgent};
use noland_state_core::*;
use noland_storage::{read_pack_index, LocalStorage, SharedStorageProvider};
use noland_testkit::{launch_mutator, Harness};
use std::sync::Arc;

#[tokio::test]
async fn backup_commit_restore_roundtrip() {
    let harness = Harness::new();
    let exe = harness.home.join("bin/example-game");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, b"game").unwrap();
    harness.write_desktop("example-game", "Example Game", &exe);
    let data = harness.write_minecraft_like();

    let config = AgentConfig::isolated(harness.root.clone());
    let mut isolated = config;
    isolated.home = harness.home.clone();
    isolated.paths.ensure_dirs().unwrap();
    let agent = StateAgent::boot(isolated).unwrap();
    agent.discover().unwrap();

    let app_id = AppId::desktop("example-game");
    let session = AppSession::new(app_id.clone(), 4242, SessionSource::DesktopEntry);
    agent
        .db
        .upsert_app(&AppIdentity::new(app_id.clone(), "Example Game"))
        .unwrap();
    agent.db.insert_session(&session).unwrap();
    agent
        .db
        .add_known_root(&app_id, "state", &data.to_string_lossy())
        .unwrap();

    let mut engine = AttributionEngine::new(
        &agent.db,
        LogicalRootMap::from_home(&harness.home),
        agent.config.paths.clone(),
    );
    engine.known_apps = agent.db.list_apps().unwrap();
    let hub = ObserverHub::new(agent.metrics.clone());
    let save = data.join("saves/world/level.dat");
    let pid = launch_mutator(&save, b"world-v1");
    let _ = pid;
    hub.inject_process(process_exec(session.root_pid, 1, &exe));
    hub.inject_fs(fs_event(FsEventKind::Write, session.root_pid, &save));
    hub.inject_fs(fs_event(
        FsEventKind::Write,
        session.root_pid,
        harness.home.join(".config/example-game/options.txt"),
    ));
    let _ = Arc::new(());
    noland_attribution::process_hub_events(&mut engine, &hub).unwrap();

    let master = MasterKey::generate();
    let cloud = harness.root.join("cloud");
    let manifest = run_backup_to_local(
        &agent,
        &app_id,
        BackupMode::PersonalState,
        cloud.clone(),
        &master,
    )
    .await
    .unwrap();
    assert!(!manifest.files.is_empty());
    assert_eq!(manifest.hash.algorithm, "blake3");
    assert_eq!(manifest.chunking.min, constants::FASTCDC_MIN);
    assert_eq!(manifest.chunking.avg, constants::FASTCDC_AVG);
    assert_eq!(manifest.chunking.max, constants::FASTCDC_MAX);

    let committed = cloud.join(format!(
        "bundles/{}/{}/COMMITTED",
        app_id.storage_safe(),
        manifest.bundle_id
    ));
    assert!(committed.exists(), "COMMITTED marker must exist");

    // Destroy local state and restore onto a fresh home.
    std::fs::remove_dir_all(&data).unwrap();
    std::fs::remove_dir_all(harness.home.join(".config/example-game")).ok();
    let fresh_home = harness.root.join("fresh-home");
    std::fs::create_dir_all(fresh_home.join(".local/share")).unwrap();
    std::fs::create_dir_all(fresh_home.join(".config")).unwrap();
    let fresh_roots = LogicalRootMap::from_home(&fresh_home);

    let storage = LocalStorage::new(&cloud);
    storage.ensure_root().await.unwrap();
    let plan = prepare_restore(
        &storage,
        &master,
        &agent.config.paths,
        &app_id,
        manifest.bundle_id,
        RestoreMode::PersonalState,
    )
    .await
    .unwrap();

    let index = read_pack_index(&storage, &master, &app_id, manifest.bundle_id)
        .await
        .unwrap();
    download_and_verify(&storage, &master, &plan, &index)
        .await
        .unwrap();

    let mut restore = RestoreTransaction::new(&plan, &fresh_roots, Some(&agent.db));
    let report = restore.publish_to(RestoreTarget::Complete).unwrap();
    assert!(report.published_entries > 0);
    restore.commit().unwrap();

    assert_eq!(
        std::fs::read(fresh_home.join(".local/share/example-game/saves/world/level.dat")).unwrap(),
        b"world-v1"
    );
    assert_eq!(
        std::fs::read(fresh_home.join(".config/example-game/options.txt")).unwrap(),
        b"render=fancy"
    );
    assert!(!plan.staging.exists());
}
