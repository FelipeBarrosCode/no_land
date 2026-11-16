//! Local synthetic payloads only. These prove capture/restore of the three
//! distinct runtime roots, not receipt migration or successful application launch.
use super::*;
use crate::{backup::run_backup, AgentConfig};
use noland_crypto::MasterKey;
use noland_restore::{prepare_restore, restore_pipelined, PipelineOptions};
use noland_storage::{read_pack_index, LocalStorage};
use noland_storage::{
    Health, ProviderRootIdentity, RemoteEntry, RemoteKey, RemoteMeta, SharedStorageProvider,
};
use serde_json::{json, Value};
use std::fs::{self, File, FileTimes};
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::sync::atomic::{AtomicBool, Ordering};
use uuid::Uuid;

fn directory(path: &Path) {
    fs::create_dir_all(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn write(path: &Path, bytes: &[u8]) {
    directory(path.parent().unwrap());
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn identity(path: &Path) -> Value {
    let meta = fs::symlink_metadata(path).unwrap();
    json!({
        "dev": meta.dev(), "ino": meta.ino(), "size": meta.len(),
        "mtime": [meta.mtime(), meta.mtime_nsec()],
        "ctime": [meta.ctime(), meta.ctime_nsec()], "mode": meta.mode()
    })
}

struct Fixture {
    root: PathBuf,
    agent: StateAgent,
    app: AppId,
    source: PathBuf,
    view: PathBuf,
    private_home: PathBuf,
    record: PathBuf,
    receipt: PathBuf,
}

impl Fixture {
    fn new(workspace: bool, appimage: bool) -> Self {
        let root = std::env::temp_dir().join(format!("runtime-capture-{}", Uuid::new_v4()));
        directory(&root);
        let root = fs::canonicalize(root).unwrap();
        let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
        let base = agent.config.home.join(".local/share/noland-runtime");
        let source = base.join("app-data/original/Games/Hookshot");
        let view = base
            .join(if workspace { "workspaces" } else { "portable" })
            .join("current/view");
        let private_home = base.join("app-data/current");
        let record = base.join("apps/current.json");
        let receipt = view.parent().unwrap().join("manifest.json");
        let mut executable = vec![0; 20];
        executable[..4].copy_from_slice(b"\x7fELF");
        executable[4] = 2;
        executable[5] = 1;
        executable[18] = 62;
        if appimage {
            executable[8..11].copy_from_slice(b"AI\x02");
        }
        executable.extend_from_slice(b"fixture original executable");
        write(&source.join("hookshot"), &executable);
        fs::set_permissions(source.join("hookshot"), fs::Permissions::from_mode(0o700)).unwrap();
        write(&source.join("romfs/asset.bin"), b"fixture immutable asset");
        write(&source.join("user/save/progress.sav"), b"original-save");
        write(&source.join("cache.bin"), b"original-cache");
        write(&source.join("archives/game.zip"), b"preserved original ZIP");
        write(
            &source.join("backup/archive.nup"),
            b"preserved original backup",
        );
        write(&source.join("mods/custom.bin"), b"preserved original mod");
        write(
            &private_home.join(".config/game/settings.ini"),
            b"volume=42",
        );
        write(
            &private_home.join(".cache/game/shaders.bin"),
            b"private home cache",
        );
        write(&view.join("user/save/progress.sav"), b"private-save-v1");
        write(&view.join("cache.bin"), b"private-cache-v1");
        write(&view.join("new-private.txt"), b"new private data");
        if workspace {
            write(&view.join("bin/main"), b"private updated executable");
            fs::set_permissions(view.join("bin/main"), fs::Permissions::from_mode(0o700)).unwrap();
            write(&view.join("romfs/asset.bin"), b"private workspace asset");
            if appimage {
                symlink("bin/main", view.join("AppRun")).unwrap();
            }
            write(&receipt, &serde_json::to_vec(&json!({
                "schema": 1, "appId": "current", "originalFile": source.join("hookshot"),
                "appimage": appimage, "executable": if appimage { "AppRun" } else { "bin/main" },
                "source": if appimage { base.join("cache").join(format!("appimage-{}/squashfs-root", "a".repeat(64))) } else { source.clone() },
                "importSnapshotSha256": "b".repeat(64), "legacyPaths": [],
                "stats": {"files": 4, "bytes": 100, "reflinkFiles": 0}
            })).unwrap());
        } else {
            write(&view.join("hookshot"), b"");
            for name in ["romfs", "archives", "backup", "mods"] {
                symlink(
                    Path::new("/home/runtime/.noland-source").join(name),
                    view.join(name),
                )
                .unwrap();
            }
            fn seed(source: &Path, relative: &Path, out: &mut serde_json::Map<String, Value>) {
                let path = source.join(relative);
                if path.is_dir() {
                    for entry in fs::read_dir(&path).unwrap() {
                        seed(source, &relative.join(entry.unwrap().file_name()), out);
                    }
                }
                out.insert(relative.to_str().unwrap().into(), identity(&path));
            }
            let mut seeds = serde_json::Map::new();
            seed(&source, Path::new("user"), &mut seeds);
            seed(&source, Path::new("cache.bin"), &mut seeds);
            let entries = fs::read_dir(&source)
                .unwrap()
                .map(|entry| {
                    let entry = entry.unwrap();
                    (
                        entry.file_name().into_string().unwrap(),
                        identity(&entry.path()),
                    )
                })
                .collect::<serde_json::Map<_, _>>();
            write(
                &receipt,
                &serde_json::to_vec(&json!({
                    "source": source, "source_identity": identity(&source),
                    "writable": ["user", "cache.bin"], "entries": entries, "seeds": seeds
                }))
                .unwrap(),
            );
        }
        write(
            &record,
            &serde_json::to_vec(&json!({
                "id": "current", "name": "Not identity authority", "file": source.join("hookshot"),
                "profile": "ubuntu24-x86_64", "appimage": appimage,
                "writableApp": workspace,
                "writablePaths": if workspace { vec![] } else { vec!["user", "cache.bin"] }
            }))
            .unwrap(),
        );
        // A different app's control record can be enumerated for exact matching,
        // but neither its private HOME nor its payload may become scan roots.
        write(
            &base.join("apps/other.json"),
            &serde_json::to_vec(&json!({
                "id": "other", "file": agent.config.home.join("Games/Other/game"),
                "profile": "ubuntu24-x86_64", "appimage": false
            }))
            .unwrap(),
        );
        write(
            &base.join("app-data/other/.ssh/fixture-key"),
            b"synthetic foreign data",
        );
        write(
            &base.join("images/fixture/secret"),
            b"synthetic infrastructure",
        );
        let app = AppId::exe("hookshot", "0-0");
        agent
            .db
            .upsert_app(&AppIdentity {
                canonical_executable: Some(source.join("hookshot")),
                launcher: Some(LauncherKind::Native),
                ..AppIdentity::new(app.clone(), "Hookshot")
            })
            .unwrap();
        agent
            .db
            .add_known_root(&app, "install", source.to_str().unwrap())
            .unwrap();
        Self {
            root,
            agent,
            app,
            source,
            view,
            private_home,
            record,
            receipt,
        }
    }

    fn index_historical_path(&self, path: &Path, file_type: Option<&str>) -> PathRecord {
        let logical = self.agent.roots.lock().classify(path).unwrap();
        let record = PathRecord {
            path_id: 0,
            canonical_path: path.to_str().unwrap().into(),
            logical_root: Some(logical.logical_root.as_token()),
            relative_path: Some(logical.relative_path),
            file_type: file_type.map(str::to_owned),
            inode: None,
            mount_id: None,
            size: None,
            mtime_ns: None,
            mode: None,
            uid: None,
            gid: None,
            content_hash: None,
            last_scanned_at: Some(1),
        };
        let now = Utc::now();
        let association = PathAssociation {
            app_id: self.app.clone(),
            path_id: 0,
            confidence: CONF_REPEATED,
            evidence: vec![Evidence::new(EvidenceKind::ReconciliationDelta)],
            persistence_class: PersistenceClass::PersistentState,
            semantic_role: SemanticRole::UserState,
            first_seen_at: now,
            last_seen_at: now,
        };
        self.agent
            .db
            .upsert_scanned_paths(&[(record.clone(), association, None)])
            .unwrap();
        record
    }

    fn index_legacy_ancestors(&self) -> [PathBuf; 2] {
        let games = self.source.parent().unwrap().to_path_buf();
        let original_home = games.parent().unwrap().to_path_buf();
        let ancestors = [original_home, games];
        for path in &ancestors {
            self.index_historical_path(path, Some("directory"));
        }
        ancestors
    }

    async fn capture(&self, storage: &LocalStorage, key: &MasterKey) -> Result<BundleManifest> {
        self.capture_with_operation(storage, key)
            .await
            .map(|(manifest, _)| manifest)
    }

    async fn capture_with_operation(
        &self,
        storage: &LocalStorage,
        key: &MasterKey,
    ) -> Result<(BundleManifest, Uuid)> {
        let id = Uuid::new_v4();
        let manifest = run_backup(
            &self.agent,
            &self.app,
            BackupMode::CompleteApplication,
            BackupPerformanceMode::Balanced,
            storage,
            key,
            Some(id),
        )
        .await?;
        Ok((manifest, id))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn file<'a>(manifest: &'a BundleManifest, path: &Path) -> &'a ManifestFile {
    manifest
        .files
        .iter()
        .find(|file| file.source_path_hint.as_deref() == path.to_str())
        .unwrap()
}

#[tokio::test]
async fn exact_runtime_scope_captures_reuses_and_restores_private_data_after_exit() {
    for (workspace, appimage) in [(false, false), (true, false), (true, true)] {
        let f = Fixture::new(workspace, appimage);
        // Match historical directory observations above the original install,
        // which lives in a different private HOME from the current runtime.
        let legacy_ancestors = f.index_legacy_ancestors();
        let siblings = [
            legacy_ancestors[0].join(".config/unbound-fixture.ini"),
            legacy_ancestors[1].join("SiblingGame/progress.sav"),
        ];
        for path in &siblings {
            write(path, b"unbound sibling data");
        }
        let storage = LocalStorage::new(f.root.join("cloud"));
        let key = MasterKey::generate();
        let first = f.capture(&storage, &key).await.unwrap();
        assert_eq!(first.app.app_id, f.app);
        assert_eq!(
            file(&first, &f.view.join("user/save/progress.sav")).file_hash,
            noland_cas::blake3_hex(b"private-save-v1")
        );
        for path in [
            f.view.join("cache.bin"),
            f.view.join("new-private.txt"),
            f.private_home.join(".config/game/settings.ini"),
            f.private_home.join(".cache/game/shaders.bin"),
            f.record.clone(),
            f.receipt.clone(),
            f.source.join("archives/game.zip"),
            f.source.join("backup/archive.nup"),
            f.source.join("mods/custom.bin"),
        ] {
            file(&first, &path);
        }
        assert!(!first
            .files
            .iter()
            .any(|file| file
                .source_path_hint
                .as_deref()
                .is_some_and(|path| path.contains("/app-data/other/")
                    || path.contains("/images/")
                    || path.ends_with("/apps/other.json"))));
        if !workspace {
            assert!(!first
                .files
                .iter()
                .any(|file| file.source_path_hint.as_deref() == f.view.join("hookshot").to_str()));
            assert!(!first
                .files
                .iter()
                .any(|file| file.source_path_hint.as_deref() == f.view.join("romfs").to_str()));
        }
        let (second, operation_id) = f.capture_with_operation(&storage, &key).await.unwrap();
        assert_eq!(
            second.files.len(),
            first.files.len(),
            "repeat scans must not widen a control-file association to its shared parent"
        );
        assert!(!second
            .files
            .iter()
            .any(|file| file
                .source_path_hint
                .as_deref()
                .is_some_and(|path| path.contains("/app-data/other/")
                    || path.contains("/images/")
                    || path.ends_with("/apps/other.json"))));
        assert_eq!(
            file(&second, &f.view.join("user/save/progress.sav")).file_hash,
            file(&first, &f.view.join("user/save/progress.sav")).file_hash
        );
        let metrics = f
            .agent
            .db
            .get_operation_metrics(operation_id)
            .unwrap()
            .unwrap();
        assert!(metrics.num_files_skipped_fast_identity > 0,
            "unchanged immutable originals must retain fast reuse ({workspace}, {appimage}): {metrics:?}");
        assert!(
            metrics.num_files_rehashed > 0,
            "private files must be verified even without events"
        );
        assert_eq!(
            metrics.num_chunks_created, 0,
            "verifying unchanged private files must reuse their existing payload"
        );
        assert!(metrics.num_chunks_reused > 0);
        // Deliberately make a same-size, same-mtime private write, without any
        // process/session/event. Complete backup must not reuse its old bytes.
        let save = f.view.join("user/save/progress.sav");
        let modified = fs::metadata(&save).unwrap().modified().unwrap();
        fs::write(&save, b"private-save-v2").unwrap();
        File::open(&save)
            .unwrap()
            .set_times(FileTimes::new().set_modified(modified))
            .unwrap();
        fs::write(f.view.join("cache.bin"), b"private-cache-v2-more").unwrap();
        let changed = f.capture(&storage, &key).await.unwrap();
        assert_eq!(
            file(&changed, &save).file_hash,
            noland_cas::blake3_hex(b"private-save-v2")
        );
        assert_eq!(
            file(&changed, &f.view.join("cache.bin")).file_hash,
            noland_cas::blake3_hex(b"private-cache-v2-more")
        );
        assert_eq!(
            fs::read(f.source.join("user/save/progress.sav")).unwrap(),
            b"original-save"
        );
        assert_eq!(
            fs::read(f.source.join("cache.bin")).unwrap(),
            b"original-cache"
        );
        assert_eq!(
            f.agent.db.known_roots(Some(&f.app)).unwrap().len(),
            1,
            "runtime roots must not outlive their fresh validation as persistent authority"
        );
        for manifest in [&first, &second, &changed] {
            assert!(!manifest.files.iter().any(|file| {
                file.source_path_hint.as_deref().is_some_and(|path| {
                    legacy_ancestors.contains(&PathBuf::from(path))
                        || siblings.contains(&PathBuf::from(path))
                })
            }));
        }
        let associations = f.agent.db.associations_for_app(&f.app).unwrap();
        for ancestor in &legacy_ancestors {
            let (record, _) = associations
                .iter()
                .find(|(record, _)| Path::new(&record.canonical_path) == ancestor)
                .unwrap();
            assert_eq!(record.file_type.as_deref(), Some("directory"));
            assert_eq!(
                record.last_scanned_at,
                Some(1),
                "structural ancestors must not be traversed or reindexed"
            );
        }
        let states = f.agent.db.list_file_states(&f.app, None).unwrap();
        for path in legacy_ancestors.iter().chain(&siblings) {
            assert!(!states
                .iter()
                .any(|state| state.canonical_path.as_deref() == path.to_str()));
        }
        for sibling in &siblings {
            assert!(!associations
                .iter()
                .any(|(record, _)| Path::new(&record.canonical_path) == sibling));
            assert_eq!(fs::read(sibling).unwrap(), b"unbound sibling data");
        }

        let restored_home = f.root.join("restored-home");
        let roots = LogicalRootMap::from_home(&restored_home);
        let plan = prepare_restore(
            &storage,
            &key,
            &f.agent.config.paths,
            &f.app,
            changed.bundle_id,
            RestoreMode::CompleteApplication,
        )
        .await
        .unwrap();
        let index = read_pack_index(&storage, &key, &f.app, changed.bundle_id)
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
        let destination =
            |path: &Path| restored_home.join(path.strip_prefix(&f.agent.config.home).unwrap());
        for sibling in &siblings {
            assert!(!destination(sibling).exists());
        }
        assert_eq!(fs::read(destination(&save)).unwrap(), b"private-save-v2");
        assert_eq!(
            fs::read(destination(&f.view.join("cache.bin"))).unwrap(),
            b"private-cache-v2-more"
        );
        assert_eq!(
            fs::read(destination(&f.private_home.join(".cache/game/shaders.bin"))).unwrap(),
            b"private home cache"
        );
        assert_eq!(
            fs::read(destination(&f.source.join("user/save/progress.sav"))).unwrap(),
            b"original-save"
        );
        assert_eq!(
            fs::read(destination(&f.record)).unwrap(),
            fs::read(&f.record).unwrap()
        );
        assert_eq!(
            fs::read(destination(&f.receipt)).unwrap(),
            fs::read(&f.receipt).unwrap()
        );
        // The restored receipt still names the original machine/generation.
        // This is intentionally NOT a claim of automatic runtime launchability.
    }
}

#[test]
fn structural_runtime_install_ancestor_requires_stored_directory_and_exact_source_root() {
    let f = Fixture::new(false, false);
    let ancestor = f.source.parent().unwrap();
    let mut record = f.index_historical_path(ancestor, Some("directory"));
    let binding = validated_runtime_binding(&f.agent, &f.app)
        .unwrap()
        .unwrap();
    let roots = [f.source.clone()];
    assert!(!binding.contains(ancestor));
    assert!(is_structural_runtime_install_ancestor(&record, &binding, &roots).unwrap());

    for file_type in [None, Some("file"), Some("symlink")] {
        record.file_type = file_type.map(str::to_owned);
        assert!(!is_structural_runtime_install_ancestor(&record, &binding, &roots).unwrap());
    }
    record.file_type = Some("directory".into());
    for roots in [vec![], vec![ancestor.join("OtherGame")]] {
        assert!(!is_structural_runtime_install_ancestor(&record, &binding, &roots).unwrap());
    }
    record.canonical_path = f.source.to_str().unwrap().into();
    assert!(!is_structural_runtime_install_ancestor(&record, &binding, &roots).unwrap());
    for suffix in ["/.", "/../Games"] {
        record.canonical_path = format!("{}{suffix}", ancestor.display());
        assert!(!is_structural_runtime_install_ancestor(&record, &binding, &roots).unwrap());
    }
}

#[tokio::test]
async fn unbound_historical_private_payload_and_nonancestor_directories_still_fail_closed() {
    for file_type in ["file", "symlink", "directory"] {
        let f = Fixture::new(false, false);
        let ancestors = f.index_legacy_ancestors();
        let path = ancestors[0].join("unbound-state");
        match file_type {
            "file" => write(&path, b"unbound private save"),
            "symlink" => symlink(f.view.join("user/save/progress.sav"), &path).unwrap(),
            "directory" => directory(&path),
            _ => unreachable!(),
        }
        f.index_historical_path(&path, Some(file_type));
        let binding = validated_runtime_binding(&f.agent, &f.app)
            .unwrap()
            .unwrap();
        assert!(!binding.contains(&path));
        assert!(!path.starts_with(&f.source) && !f.source.starts_with(&path));

        let storage = LocalStorage::new(f.root.join("cloud"));
        let error = f
            .capture(&storage, &MasterKey::generate())
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("runtime application paths lost their validated ownership binding"),
            "{file_type}: {error}"
        );
        assert!(f.agent.db.latest_commit(&f.app).unwrap().is_none());
        assert_eq!(
            f.agent.db.associations_for_app(&f.app).unwrap().len(),
            3,
            "rejection must not clear historical ownership evidence"
        );
    }
}

#[tokio::test]
async fn structural_runtime_install_ancestor_rejects_replaced_or_missing_components() {
    for replacement in ["file", "symlink", "parent-symlink", "missing"] {
        let f = Fixture::new(false, false);
        let ancestor = f.source.parent().unwrap();
        let record = f.index_historical_path(ancestor, Some("directory"));
        let binding = validated_runtime_binding(&f.agent, &f.app)
            .unwrap()
            .unwrap();
        let roots = [f.source.clone()];
        assert!(is_structural_runtime_install_ancestor(&record, &binding, &roots).unwrap());

        // Replace only synthetic fixture structure after binding validation.
        // Test the helper's no-follow check as well as fresh capture validation.
        let replaced = if replacement == "parent-symlink" {
            ancestor.parent().unwrap()
        } else {
            ancestor
        };
        let detached = f.root.join("detached-original");
        fs::rename(replaced, &detached).unwrap();
        match replacement {
            "file" => write(replaced, b"not a directory"),
            "symlink" | "parent-symlink" => symlink(&detached, replaced).unwrap(),
            "missing" => {}
            _ => unreachable!(),
        }
        let result = is_structural_runtime_install_ancestor(&record, &binding, &roots);
        match replacement {
            "file" | "symlink" => assert!(!result.unwrap(), "{replacement}"),
            _ => assert!(result.is_err(), "{replacement} must fail closed"),
        }
        let storage = LocalStorage::new(f.root.join("cloud"));
        assert!(
            f.capture(&storage, &MasterKey::generate()).await.is_err(),
            "{replacement} must prevent capture"
        );
        assert!(f.agent.db.latest_commit(&f.app).unwrap().is_none());
    }
}

#[tokio::test]
async fn historical_ancestor_directories_cannot_replace_required_runtime_registration() {
    let f = Fixture::new(false, false);
    f.index_legacy_ancestors();
    assert!(validated_runtime_binding(&f.agent, &f.app)
        .unwrap()
        .is_some());
    fs::remove_file(&f.record).unwrap();
    let storage = LocalStorage::new(f.root.join("cloud"));
    let error = f
        .capture(&storage, &MasterKey::generate())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("original-only backup refused"));
    assert!(f.agent.db.latest_commit(&f.app).unwrap().is_none());
    assert_eq!(f.agent.db.associations_for_app(&f.app).unwrap().len(), 2);
}

#[tokio::test]
async fn private_deletion_does_not_reintroduce_original_and_removed_binding_fails_closed() {
    let f = Fixture::new(false, false);
    let storage = LocalStorage::new(f.root.join("cloud"));
    let key = MasterKey::generate();
    f.capture(&storage, &key).await.unwrap();
    fs::remove_file(f.view.join("cache.bin")).unwrap();
    let after = f.capture(&storage, &key).await.unwrap();
    assert!(!after
        .files
        .iter()
        .any(|file| file.source_path_hint.as_deref() == f.view.join("cache.bin").to_str()));
    file(&after, &f.source.join("cache.bin"));
    assert_eq!(
        fs::read(f.source.join("cache.bin")).unwrap(),
        b"original-cache"
    );
    fs::remove_file(&f.record).unwrap();
    assert!(
        f.capture(&storage, &key).await.is_err(),
        "loss of binding must not silently regress to an original-only backup"
    );
}

#[tokio::test]
async fn required_registration_loss_before_first_capture_cannot_fall_back_to_original() {
    for (workspace, appimage) in [(false, false), (true, false), (true, true)] {
        let f = Fixture::new(workspace, appimage);
        let identities = noland_discovery::registered_runtime_identities(
            &f.agent.config.home,
            &f.agent.db.list_apps().unwrap(),
        )
        .unwrap();
        assert_eq!(identities.len(), 1);
        assert_eq!(identities[0].app_id, f.app);
        let source = f.source.join("hookshot");
        // Seed the same sticky, non-authoritative requirement that successful
        // registration discovery records. No private path has been indexed yet.
        f.agent
            .db
            .require_runtime_binding(&f.app, source.to_str().unwrap())
            .unwrap();
        assert!(f.agent.db.runtime_binding_required(&f.app).unwrap());
        assert!(f.agent.db.associations_for_app(&f.app).unwrap().is_empty());
        assert!(f
            .agent
            .db
            .list_file_states(&f.app, None)
            .unwrap()
            .is_empty());
        let record = fs::read(&f.record).unwrap();
        fs::remove_file(&f.record).unwrap();

        let error = validated_runtime_binding(&f.agent, &f.app).unwrap_err();
        assert!(error.to_string().contains("original-only backup refused"));
        assert!(known_install_roots(&f.agent, &f.app).is_err());
        let before = f.agent.metrics.snapshot().reconciliations_total;
        let reconcile: [fn(&StateAgent, &AppId) -> Result<usize>; 4] = [
            reconcile_app_for_complete_backup,
            reconcile_app_for_refresh,
            reconcile_app,
            reconcile_install_roots,
        ];
        for reconcile in reconcile {
            assert!(reconcile(&f.agent, &f.app).is_err());
        }
        assert_eq!(f.agent.metrics.snapshot().reconciliations_total, before);
        let storage = LocalStorage::new(f.root.join("cloud"));
        let key = MasterKey::generate();
        assert!(f.capture(&storage, &key).await.is_err());
        assert!(f.agent.db.latest_commit(&f.app).unwrap().is_none());
        assert!(f.agent.db.associations_for_app(&f.app).unwrap().is_empty());
        assert!(f
            .agent
            .db
            .list_file_states(&f.app, None)
            .unwrap()
            .is_empty());
        assert!(f.agent.db.runtime_binding_required(&f.app).unwrap());

        // Repair restores access to the validated private data; neither failed
        // nor successful captures clear the sticky registration requirement.
        write(&f.record, &record);
        assert!(validated_runtime_binding(&f.agent, &f.app)
            .unwrap()
            .is_some());
        let captured = f.capture(&storage, &key).await.unwrap();
        assert_eq!(
            file(&captured, &f.view.join("user/save/progress.sav")).file_hash,
            noland_cas::blake3_hex(b"private-save-v1")
        );
        file(&captured, &f.private_home.join(".cache/game/shaders.bin"));
        assert!(f.agent.db.runtime_binding_required(&f.app).unwrap());
        assert_eq!(
            fs::read(f.source.join("user/save/progress.sav")).unwrap(),
            b"original-save"
        );
    }
}

#[test]
fn validated_binding_remembers_requirement_before_indexing_or_capture() {
    for (workspace, appimage) in [(false, false), (true, false), (true, true)] {
        let f = Fixture::new(workspace, appimage);
        assert!(!f.agent.db.runtime_binding_required(&f.app).unwrap());
        assert!(validated_runtime_binding(&f.agent, &f.app)
            .unwrap()
            .is_some());
        assert!(f.agent.db.runtime_binding_required(&f.app).unwrap());
        assert!(f.agent.db.associations_for_app(&f.app).unwrap().is_empty());
        fs::remove_file(&f.record).unwrap();
        assert!(validated_runtime_binding(&f.agent, &f.app).is_err());
        assert!(f.agent.db.runtime_binding_required(&f.app).unwrap());
    }
}

/// Deterministic local-only gate between scope reconciliation and freezing the
/// per-file capture identities. All provider I/O stays in this fixture's root.
struct RuntimeTransitionStorage {
    inner: LocalStorage,
    db: noland_state_db::StateDb,
    operation: Uuid,
    streaming: bool,
    fired: AtomicBool,
    fail_manifest_once: AtomicBool,
    transition: Box<dyn Fn() + Send + Sync>,
}

#[async_trait::async_trait]
impl SharedStorageProvider for RuntimeTransitionStorage {
    async fn health_check(&self) -> Result<Health> {
        self.inner.health_check().await
    }
    async fn ensure_root(&self) -> Result<()> {
        self.inner.ensure_root().await
    }
    async fn stat(&self, key: &RemoteKey) -> Result<Option<RemoteMeta>> {
        self.inner.stat(key).await
    }
    async fn upload_immutable(&self, local: &Path, key: &RemoteKey) -> Result<RemoteMeta> {
        self.inner.upload_immutable(local, key).await
    }
    async fn download(&self, key: &RemoteKey, dest: &Path) -> Result<()> {
        self.inner.download(key, dest).await
    }
    async fn list_prefix(&self, prefix: &RemoteKey) -> Result<Vec<RemoteEntry>> {
        self.inner.list_prefix(prefix).await
    }
    async fn delete_object(&self, key: &RemoteKey) -> Result<()> {
        self.inner.delete_object(key).await
    }
    async fn put_small_versioned(
        &self,
        bytes: bytes::Bytes,
        key: &RemoteKey,
    ) -> Result<RemoteMeta> {
        if key.as_str().ends_with("/manifest.enc")
            && self.fail_manifest_once.swap(false, Ordering::SeqCst)
        {
            return Err(StateError::Storage(
                "fixture manifest publication failure".into(),
            ));
        }
        self.inner.put_small_versioned(bytes, key).await
    }
    fn storage_identity(&self) -> Option<ProviderRootIdentity> {
        if self
            .db
            .get_operation_progress(self.operation)
            .unwrap()
            .is_some_and(|progress| progress.phase == "hashing")
            && !self.fired.swap(true, Ordering::SeqCst)
        {
            (self.transition)();
        }
        if self.streaming {
            self.inner.storage_identity()
        } else {
            None
        }
    }
}

#[tokio::test]
async fn runtime_scope_transition_before_capture_identity_freeze_cannot_commit() {
    for streaming in [false, true] {
        for initially_registered in [false, true] {
            let f = Fixture::new(false, false);
            let record = f.record.clone();
            let registration = if initially_registered {
                let base = f.agent.config.home.join(".local/share/noland-runtime");
                let full_view = base.join("workspaces/current/view");
                write(&full_view.join("bin/main"), b"new workspace executable");
                fs::set_permissions(
                    full_view.join("bin/main"),
                    fs::Permissions::from_mode(0o700),
                )
                .unwrap();
                write(
                    &full_view.join("user/save/progress.sav"),
                    b"new workspace private save",
                );
                write(
                    &base.join("workspaces/current/manifest.json"),
                    &serde_json::to_vec(&json!({
                        "schema": 1, "appId": "current",
                        "originalFile": f.source.join("hookshot"), "appimage": false,
                        "executable": "bin/main", "source": f.source,
                        "importSnapshotSha256": "c".repeat(64),
                        "legacyPaths": ["user", "cache.bin"],
                        "stats": {"files": 2, "bytes": 60, "reflinkFiles": 0}
                    }))
                    .unwrap(),
                );
                serde_json::to_vec(&json!({
                    "id": "current", "file": f.source.join("hookshot"),
                    "profile": "ubuntu24-x86_64", "appimage": false,
                    "writableApp": true, "writablePaths": []
                }))
                .unwrap()
            } else {
                let registration = fs::read(&f.record).unwrap();
                fs::remove_file(&f.record).unwrap();
                registration
            };
            let operation = Uuid::new_v4();
            let storage = RuntimeTransitionStorage {
                inner: LocalStorage::new(f.root.join("cloud")),
                db: f.agent.db.clone(),
                operation,
                streaming,
                fired: AtomicBool::new(false),
                fail_manifest_once: AtomicBool::new(false),
                transition: Box::new(move || write(&record, &registration)),
            };
            let result = run_backup(
                &f.agent,
                &f.app,
                BackupMode::CompleteApplication,
                BackupPerformanceMode::Balanced,
                &storage,
                &MasterKey::generate(),
                Some(operation),
            )
            .await;
            assert!(storage.fired.load(Ordering::SeqCst));
            let error = result.unwrap_err();
            assert!(
                error.to_string().contains("scope changed during capture"),
                "{streaming}/{initially_registered}: {error}"
            );
            // The replacement is valid. Rejection must come from a scope
            // generation change, not from a forged/partial test registration.
            let current = validated_runtime_binding(&f.agent, &f.app)
                .unwrap()
                .unwrap();
            assert_eq!(current.selected_view, !initially_registered);
            assert!(f.agent.db.runtime_binding_required(&f.app).unwrap());
            assert!(f.agent.db.latest_commit(&f.app).unwrap().is_none());
            assert!(!storage
                .inner
                .list_prefix(&RemoteKey::new(""))
                .await
                .unwrap()
                .iter()
                .any(|entry| entry.key.as_str().ends_with(COMMITTED_MARKER)));
            assert!(f
                .agent
                .db
                .list_dirty_apps()
                .unwrap()
                .iter()
                .any(|dirty| dirty.app_id == f.app && dirty.requires_reconciliation));
            assert_eq!(
                fs::read(f.source.join("user/save/progress.sav")).unwrap(),
                b"original-save"
            );
        }
    }
}

#[tokio::test]
async fn coherent_frozen_retry_does_not_recapture_a_later_live_runtime_layout() {
    let f = Fixture::new(false, false);
    let operation = Uuid::new_v4();
    let storage = RuntimeTransitionStorage {
        inner: LocalStorage::new(f.root.join("cloud")),
        db: f.agent.db.clone(),
        operation,
        streaming: true,
        fired: AtomicBool::new(false),
        fail_manifest_once: AtomicBool::new(true),
        transition: Box::new(|| {}),
    };
    let master = MasterKey::generate();
    let failed = run_backup(
        &f.agent,
        &f.app,
        BackupMode::CompleteApplication,
        BackupPerformanceMode::Balanced,
        &storage,
        &master,
        Some(operation),
    )
    .await
    .unwrap_err();
    assert!(failed
        .to_string()
        .contains("fixture manifest publication failure"));
    assert!(f
        .agent
        .config
        .paths
        .packs
        .join("resume")
        .join(format!("{operation}.enc"))
        .is_file());
    assert!(f.agent.db.latest_commit(&f.app).unwrap().is_none());

    // A new capture would now fail its sticky binding requirement. The already
    // frozen revision must instead replay its old, coherent control/data bytes.
    fs::remove_file(&f.record).unwrap();
    write(
        &f.view.join("user/save/progress.sav"),
        b"later live private save",
    );
    assert!(validated_runtime_binding(&f.agent, &f.app).is_err());
    let replayed = run_backup(
        &f.agent,
        &f.app,
        BackupMode::CompleteApplication,
        BackupPerformanceMode::Balanced,
        &storage,
        &master,
        Some(operation),
    )
    .await
    .unwrap();
    assert_eq!(
        file(&replayed, &f.view.join("user/save/progress.sav")).file_hash,
        noland_cas::blake3_hex(b"private-save-v1")
    );
    file(&replayed, &f.record);
    assert_eq!(
        fs::read(f.view.join("user/save/progress.sav")).unwrap(),
        b"later live private save"
    );
    assert_eq!(
        fs::read(f.source.join("user/save/progress.sav")).unwrap(),
        b"original-save"
    );
    assert!(f.agent.db.runtime_binding_required(&f.app).unwrap());
    assert_eq!(
        f.agent.db.get_operation(operation).unwrap().unwrap().state,
        "COMPLETED"
    );
}
