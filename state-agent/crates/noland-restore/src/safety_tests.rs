use super::*;
use crate::tests::{crash_test_plan, test_dir, test_file, test_manifest, test_plan};

fn file(path: &str, bytes: &[u8]) -> ManifestFile {
    test_file(
        path,
        PersistenceClass::PersistentState,
        SemanticRole::UserState,
        bytes,
    )
}

fn deleted_manifest() -> BundleManifest {
    let mut manifest = test_manifest(Vec::new());
    manifest.tombstones.push(ManifestTombstone {
        logical_root: "$XDG_DATA_HOME".into(),
        relative_path: "game/deleted".into(),
        reason: "removed".into(),
    });
    manifest
}

#[test]
fn database_failure_rolls_back_launcher_fallback_and_preserves_original_manifest() {
    for existing in [false, true] {
        let root = test_dir("launcher-rollback");
        let mut files = vec![file("game/save", b"new save")];
        if existing {
            files.push(file("appmanifest_42.acf", b"new manifest"));
        }
        let plan = test_plan(&root, test_manifest(files));
        for (index, payload) in [b"new save".as_slice(), b"new manifest"]
            .into_iter()
            .enumerate()
        {
            if let Some(file) = plan.manifest.files.get(index) {
                fs::write(
                    restore_chunk_path(&plan, &file.chunks[0].hash).unwrap(),
                    payload,
                )
                .unwrap();
            }
        }
        let roots = LogicalRootMap::from_home(root.join("home"));
        let data = root.join("home/.local/share");
        fs::create_dir_all(data.join("game")).unwrap();
        fs::write(data.join("game/save"), b"original save").unwrap();
        let launcher = data.join("appmanifest_42.acf");
        if existing {
            fs::write(&launcher, b"original manifest\0exact bytes").unwrap();
        }
        let db = StateDb::open_in_memory().unwrap(); // Missing app forces FK failure at commit.
        let mut transaction = RestoreTransaction::new(&plan, &roots, Some(&db));
        transaction.publish_to(RestoreTarget::Complete).unwrap();
        transaction
            .publish_prerequisites(&[RestorePrerequisite {
                root: data.clone(),
                relative_path: "appmanifest_42.acf".into(),
                contents: b"fallback".to_vec(),
            }])
            .unwrap();
        assert_eq!(
            fs::read(&launcher).unwrap(),
            if existing {
                b"new manifest".as_slice()
            } else {
                b"fallback"
            }
        );
        assert!(transaction.commit().is_err());
        assert_eq!(fs::read(data.join("game/save")).unwrap(), b"original save");
        if existing {
            assert_eq!(
                fs::read(&launcher).unwrap(),
                b"original manifest\0exact bytes"
            );
        } else {
            assert!(!launcher.exists());
        }
        assert!(!plan.staging.exists());
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn tombstone_swaps_are_rejected_and_keep_recoverable_original_bytes() {
    for swap in ["directory", "file", "symlink", "parent"] {
        let root = test_dir("tombstone-swap");
        let plan = test_plan(&root, deleted_manifest());
        let roots = LogicalRootMap::from_home(root.join("home"));
        let destination = root.join("home/.local/share/game/deleted");
        let parent = destination.parent().unwrap();
        fs::create_dir_all(parent).unwrap();
        fs::write(&destination, b"original").unwrap();
        let mut transaction = RestoreTransaction::new(&plan, &roots, None);
        let (planned, tombstones) = transaction.preflight(RestoreTarget::Complete).unwrap();
        transaction.prepare_rollback(&planned, &tombstones).unwrap();
        let saved = root.join("moved");
        if swap == "parent" {
            fs::rename(parent, &saved).unwrap();
            fs::create_dir(parent).unwrap();
            fs::write(&destination, b"unrelated").unwrap();
        } else {
            fs::rename(&destination, &saved).unwrap();
            match swap {
                "directory" => {
                    fs::create_dir(&destination).unwrap();
                    fs::write(destination.join("keep"), b"unrelated").unwrap();
                }
                "symlink" => std::os::unix::fs::symlink(&saved, &destination).unwrap(),
                _ => fs::write(&destination, b"unrelated").unwrap(),
            }
        }
        assert!(tombstones[0].apply().is_err());
        assert!(
            transaction.abort().is_err(),
            "swaps must not be overwritten by rollback"
        );
        assert!(plan.staging.exists());
        assert_eq!(
            fs::read(&transaction.rollback_views[0].mappings[0].staged).unwrap(),
            b"original"
        );
        if swap == "parent" {
            assert_eq!(fs::read(&destination).unwrap(), b"unrelated");
            fs::remove_file(&destination).unwrap();
            fs::remove_dir(parent).unwrap();
            fs::rename(&saved, parent).unwrap();
        } else {
            if swap == "directory" {
                assert_eq!(fs::read(destination.join("keep")).unwrap(), b"unrelated");
                fs::remove_dir_all(&destination).unwrap();
            } else {
                fs::remove_file(&destination).unwrap();
            }
            fs::rename(&saved, &destination).unwrap();
        }
        transaction.abort().unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"original");
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn unstable_or_changed_rollback_capture_is_rejected_before_publication() {
    let root = test_dir("unstable-rollback");
    let plan = test_plan(&root, test_manifest(vec![file("game/save", b"new")]));
    let roots = LogicalRootMap::from_home(root.join("home"));
    let destination = root.join("home/.local/share/game/save");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::write(&destination, b"old").unwrap();
    let canonical = fs::canonicalize(&destination).unwrap();
    let mut view = noland_snapshot::create_view(
        &plan.staging.join("pre_restore"),
        std::slice::from_ref(&canonical),
        false,
    )
    .unwrap();
    view.consistency = ConsistencyKind::BestEffort;
    assert!(validate_rollback_view(&view, &[canonical]).is_err());
    let mut transaction = RestoreTransaction::new(&plan, &roots, None);
    let (planned, tombstones) = transaction.preflight(RestoreTarget::Complete).unwrap();
    let result = transaction.prepare_rollback_observed(&planned, &tombstones, |count| {
        if count == 1 {
            fs::write(&destination, b"external edit during capture")?;
        }
        Ok(())
    });
    assert!(result.is_err());
    assert!(transaction.rollback_views.is_empty());
    drop(transaction);
    assert_eq!(
        fs::read(&destination).unwrap(),
        b"external edit during capture"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn recovery_can_repeat_after_created_parent_removal_and_tombstone_restoration() {
    let root = test_dir("repeat-recovery");
    let paths = AgentPaths::from_roots(root.join("state"), root.join("run"));
    paths.ensure_dirs().unwrap();
    let mut manifest = deleted_manifest();
    manifest.files.push(file("new/deep/file", b"new"));
    let plan = crash_test_plan(&paths, manifest);
    fs::write(
        restore_chunk_path(&plan, &plan.manifest.files[0].chunks[0].hash).unwrap(),
        b"new",
    )
    .unwrap();
    let roots = LogicalRootMap::from_home(root.join("home"));
    let destination = root.join("home/.local/share/game/deleted");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::write(&destination, b"original").unwrap();
    let mut transaction = RestoreTransaction::new(&plan, &roots, None);
    transaction.publish_to(RestoreTarget::Complete).unwrap();
    let journal = journal::read(&plan.staging).unwrap();
    std::mem::forget(transaction);
    rollback_from_journal(&plan.staging, &journal).unwrap();
    assert!(!root.join("home/.local/share/new").exists());
    assert_eq!(fs::read(&destination).unwrap(), b"original");
    // Model interruption between rollback and workspace deletion.
    rollback_from_journal(&plan.staging, &journal).unwrap();
    let report = recover_interrupted_restores(&paths).unwrap();
    assert_eq!(report.workspaces_rolled_back, 1);
    assert!(report.failures.is_empty());
    fs::remove_dir_all(root).unwrap();
}
