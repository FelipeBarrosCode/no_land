use std::path::PathBuf;

use chrono::Utc;
use noland_attribution::{process_hub_events, AttributionEngine};
use noland_classifier::Classifier;
use noland_discovery::discover_desktop_apps;
use noland_observer::{fs_event, process_exec, ObserverHub};
use noland_state_core::metrics::Metrics;
use noland_state_core::*;
use noland_state_db::StateDb;
use noland_testkit::{launch_mutator, Harness};
use std::sync::Arc;

#[test]
fn launched_app_writes_are_attributed_reads_are_not_ownership() {
    let harness = Harness::new();
    let exe = harness.home.join("bin/example-game");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, b"#!/bin/sh\n").unwrap();
    harness.write_desktop("example-game", "Example Game", &exe);

    let apps = discover_desktop_apps(&harness.home);
    assert!(apps.iter().any(|a| a.display_name == "Example Game"));

    let db = StateDb::open(&harness.root.join("state.db")).unwrap();
    for app in &apps {
        db.upsert_app(app).unwrap();
    }
    let paths = AgentPaths::from_roots(harness.root.join("state"), harness.root.join("run"));
    paths.ensure_dirs().unwrap();
    let roots = LogicalRootMap::from_home(&harness.home);
    let mut engine = AttributionEngine::new(&db, roots, paths);
    engine.known_apps = apps.clone();

    let hub = ObserverHub::new(Arc::new(Metrics::default()));
    let save = harness
        .home
        .join(".local/share/example-game/saves/world/level.dat");
    let pid = launch_mutator(&save, b"world-v1");
    hub.inject_process(process_exec(pid, 1, &exe));
    hub.inject_fs(fs_event(FsEventKind::Create, pid, &save));
    hub.inject_fs(fs_event(FsEventKind::Write, pid, &save));
    hub.inject_fs(fs_event(
        FsEventKind::Read,
        pid,
        PathBuf::from("/usr/lib/libc.so.6"),
    ));

    process_hub_events(&mut engine, &hub).unwrap();

    let app_id = AppId::desktop("example-game");
    let rows = db.associations_for_app(&app_id).unwrap();
    let save_assoc = rows
        .iter()
        .find(|(p, _)| p.canonical_path.contains("level.dat"))
        .map(|(_, a)| a)
        .expect("save should be attributed");
    assert!(
        save_assoc.confidence >= CONF_DIRECT_OUTSIDE_ROOT,
        "writes must prove ownership, got {}",
        save_assoc.confidence
    );
    assert!(save_assoc.evidence.iter().any(|e| e.kind.is_mutation()));

    let libc = rows
        .iter()
        .find(|(p, _)| p.canonical_path.contains("libc.so.6"))
        .map(|(_, a)| a);
    if let Some(libc) = libc {
        assert!(
            libc.confidence <= CONF_DEPENDENCY + 0.05,
            "reads must not become ownership: {}",
            libc.confidence
        );
    }

    let clf = Classifier::new(&db, "test-image");
    clf.reclassify_app(&app_id).unwrap();
    let rows = db.associations_for_app(&app_id).unwrap();
    let save = rows
        .iter()
        .find(|(p, _)| p.canonical_path.contains("level.dat"))
        .unwrap();
    let (class, role) = clf.classify_path(&save.0, &save.1).unwrap();
    assert_eq!(class, PersistenceClass::PersistentState);
    assert_eq!(role, SemanticRole::UserState);
    let _ = Utc::now();
}
