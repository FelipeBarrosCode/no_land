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
use sha2::{Digest, Sha256};
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

fn copy_installation(source: &Path, destination: &Path) {
    let metadata = fs::symlink_metadata(source).unwrap();
    if metadata.is_dir() {
        directory(destination);
        for entry in fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            copy_installation(&entry.path(), &destination.join(entry.file_name()));
        }
    } else {
        assert!(metadata.is_file());
        write(destination, &fs::read(source).unwrap());
        if metadata.mode() & 0o111 != 0 {
            fs::set_permissions(destination, fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
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
    fn migrate_to_workspace(&mut self) -> Vec<PathBuf> {
        let old_view = self.view.clone();
        let old_receipt = self.receipt.clone();
        let base = self.agent.config.home.join(".local/share/noland-runtime");
        let view = base.join("workspaces/current/view");
        directory(&view);
        // Native imports copy all remaining original assets, but selected
        // private paths come only from their live view (including deletions).
        for entry in fs::read_dir(&self.source).unwrap() {
            let entry = entry.unwrap();
            if !["user", "cache.bin"].contains(&entry.file_name().to_str().unwrap()) {
                copy_installation(&entry.path(), &view.join(entry.file_name()));
            }
        }
        let mut entries = serde_json::Map::new();
        fn copy_private(
            old: &Path,
            new: &Path,
            root: &Path,
            entries: &mut serde_json::Map<String, Value>,
        ) {
            let meta = fs::symlink_metadata(old).unwrap();
            entries.insert(
                old.strip_prefix(root).unwrap().to_str().unwrap().into(),
                identity(old),
            );
            if meta.is_dir() {
                directory(new);
                for entry in fs::read_dir(old).unwrap() {
                    let entry = entry.unwrap();
                    copy_private(&entry.path(), &new.join(entry.file_name()), root, entries);
                }
            } else {
                assert!(meta.is_file());
                write(new, &fs::read(old).unwrap());
            }
        }
        for entry in fs::read_dir(&old_view).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            if name == "hookshot"
                || (!["user", "cache.bin"].contains(&name.to_str().unwrap())
                    && self.source.join(&name).exists())
            {
                continue;
            }
            copy_private(&entry.path(), &view.join(&name), &old_view, &mut entries);
        }
        write(&view.join("hookshot"), b"workspace fixture executable");
        fs::set_permissions(view.join("hookshot"), fs::Permissions::from_mode(0o700)).unwrap();
        write(&view.join("romfs/asset.bin"), b"workspace asset");
        let proof = serde_json::to_vec(&json!({
            "schema": 1, "appId": "current", "originalFile": self.source.join("hookshot"),
            "importSnapshotSha256": "b".repeat(64),
            "legacyManifestIdentity": identity(&old_receipt),
            "legacyViewIdentity": identity(&old_view), "entries": entries,
        }))
        .unwrap();
        let receipt = view.parent().unwrap().join("manifest.json");
        write(&view.parent().unwrap().join("legacy-import.json"), &proof);
        write(
            &receipt,
            &serde_json::to_vec(&json!({
                "schema": 1, "appId": "current", "originalFile": self.source.join("hookshot"),
                "appimage": false, "executable": "hookshot", "source": self.source,
                "importSnapshotSha256": "b".repeat(64),
                "legacyImportSha256": format!("{:x}", Sha256::digest(&proof)),
                "legacyPaths": ["user", "cache.bin"],
                "stats": {"files": 6, "bytes": 100, "reflinkFiles": 0}
            }))
            .unwrap(),
        );
        write(&self.record, &serde_json::to_vec(&json!({
            "id": "current", "name": "Not identity authority", "file": self.source.join("hookshot"),
            "profile": "ubuntu24-x86_64", "appimage": false, "writableApp": true
        })).unwrap());
        self.view = view;
        self.receipt = receipt;
        entries
            .keys()
            .map(|path| old_view.join(path))
            .chain([old_view.clone(), old_receipt])
            .collect()
    }

    fn new(workspace: bool, appimage: bool) -> Self {
        Self::with_app(workspace, appimage, AppId::exe("hookshot", "0-0"))
    }

    fn with_app(workspace: bool, appimage: bool, app: AppId) -> Self {
        let root = crate::index_test_support::root("runtime-capture");
        directory(&root);
        let root = fs::canonicalize(root).unwrap();
        assert!(
            !noland_state_core::paths::is_hard_volatile_root(&root),
            "runtime capture fixture must live outside production's hard-volatile roots"
        );
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
        if workspace && !appimage {
            copy_installation(&source, &view);
        }
        write(&view.join("user/save/progress.sav"), b"private-save-v1");
        write(&view.join("cache.bin"), b"private-cache-v1");
        write(&view.join("new-private.txt"), b"new private data");
        if workspace {
            let executable = if appimage { "bin/main" } else { "hookshot" };
            write(&view.join(executable), b"private updated executable");
            fs::set_permissions(view.join(executable), fs::Permissions::from_mode(0o700)).unwrap();
            write(&view.join("romfs/asset.bin"), b"private workspace asset");
            if appimage {
                symlink("bin/main", view.join("AppRun")).unwrap();
            }
            write(&receipt, &serde_json::to_vec(&json!({
                "schema": 1, "appId": "current", "originalFile": source.join("hookshot"),
                "appimage": appimage, "executable": if appimage { "AppRun" } else { "hookshot" },
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
        self.index_path(path, file_type, EvidenceKind::ReconciliationDelta)
    }

    fn index_path(
        &self,
        path: &Path,
        file_type: Option<&str>,
        evidence: EvidenceKind,
    ) -> PathRecord {
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
            evidence: vec![Evidence::new(evidence)],
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

fn has_file(manifest: &BundleManifest, path: &Path) -> bool {
    manifest
        .files
        .iter()
        .any(|file| file.source_path_hint.as_deref() == path.to_str())
}

#[tokio::test]
async fn steam_native_workspace_keeps_runtime_controls_home_and_anchor_without_widening_scope() {
    for migrated in [false, true] {
        let mut f = Fixture::with_app(!migrated, false, AppId::steam(480));
        if migrated {
            f.migrate_to_workspace();
        }
        let mut identity = f.agent.db.get_app(&f.app).unwrap().unwrap();
        identity.launcher = Some(LauncherKind::Steam);
        identity.steam_app_id = Some(480);
        f.agent.db.upsert_app(&identity).unwrap();
        let steam = f.agent.config.home.join(".local/share/Steam");
        let library = steam.join("steamapps");
        let ordinary_install = library.join("common/Runtime Fixture");
        let ordinary_settings = ordinary_install.join("launcher-settings.ini");
        let compat_settings = library
            .join("compatdata/480/pfx/drive_c/users/steamuser/AppData/Roaming/Game/settings.ini");
        let shaders = library.join("shadercache/480/program.bin");
        let userdata = steam.join("userdata/123/480/remote/preferences.vdf");
        for path in [&ordinary_settings, &compat_settings, &shaders, &userdata] {
            write(path, b"this Steam application's settings");
        }
        f.agent
            .db
            .add_known_root(&f.app, "install", ordinary_install.to_str().unwrap())
            .unwrap();
        {
            let mut roots = f.agent.roots.lock();
            roots.steam_root = Some(steam.clone());
            roots.steam_userdata = Some(steam.join("userdata"));
            roots
                .steam_libraries
                .insert("runtime-fixture".into(), library.clone());
        }
        let base = f.agent.config.home.join(".local/share/noland-runtime");
        let excluded = [
            library.join("common/Other Game/unbound.bin"),
            library.join("common/Proton 9.0/proton"),
            library.join("compatdata/999/pfx/drive_c/foreign-save.dat"),
            library.join("shadercache/999/program.bin"),
            steam.join("userdata/123/999/remote/preferences.vdf"),
            base.join("app-data/other/unbound-state.sav"),
            base.join("workspaces/other/view/private.bin"),
            base.join("images/fixture/secret"),
            base.join("cache/shared-extraction/private.bin"),
            f.source.parent().unwrap().join("SiblingGame/progress.sav"),
        ];
        for path in &excluded {
            write(path, b"unrelated Steam or runtime data");
            f.agent
                .db
                .mark_dirty_root(&f.app, path.parent().unwrap().to_str().unwrap(), None, true)
                .unwrap();
        }
        f.agent
            .db
            .mark_dirty_root(&f.app, f.source.to_str().unwrap(), None, true)
            .unwrap();
        let save = f.view.join("user/save/progress.sav");
        write(&save, b"current Steam runtime workspace save");
        let proof = f.receipt.parent().unwrap().join("legacy-import.json");
        let binding = validated_runtime_binding(&f.agent, &f.app)
            .unwrap()
            .unwrap();
        let scope = SteamBackupScope::load(&f.agent, &f.app).unwrap().unwrap();
        for root in binding.roots() {
            assert!(
                scope.contains(root),
                "missing runtime scope {}",
                root.display()
            );
        }
        for path in &excluded {
            assert!(!scope.contains(path), "borrowed scope {}", path.display());
        }
        assert!(!scope.contains(&base.join("apps")));
        assert!(!scope.contains(&base.join("apps/other.json")));
        assert!(!scope.contains(&base.join("workspaces")));
        assert!(!scope.contains(&base.join("app-data")));

        let storage = LocalStorage::new(f.root.join("cloud"));
        let key = MasterKey::generate();
        let manifest = f.capture(&storage, &key).await.unwrap();
        assert_eq!(manifest.app.app_id, AppId::steam(480));
        for path in [
            &f.source.join("hookshot"),
            &f.view.join("hookshot"),
            &f.view.join("archives/game.zip"),
            &f.record,
            &f.receipt,
            &f.private_home.join(".config/game/settings.ini"),
            &f.private_home.join(".cache/game/shaders.bin"),
            &ordinary_settings,
            &compat_settings,
            &shaders,
            &userdata,
        ] {
            assert!(has_file(&manifest, path), "missing {}", path.display());
        }
        assert_eq!(has_file(&manifest, &proof), migrated);
        assert_eq!(
            file(&manifest, &save).file_hash,
            noland_cas::blake3_hex(b"current Steam runtime workspace save")
        );
        assert!(manifest.files.iter().all(|file| {
            file.source_path_hint.as_deref().is_none_or(|path| {
                !Path::new(path).starts_with(&f.source)
                    || Path::new(path) == f.source.join("hookshot")
            })
        }));
        for path in excluded.iter().chain([base.join("apps/other.json")].iter()) {
            assert!(!has_file(&manifest, path), "captured {}", path.display());
        }
        let associations = f.agent.db.associations_for_app(&f.app).unwrap();
        for path in &excluded {
            assert!(!associations
                .iter()
                .any(|(record, _)| Path::new(&record.canonical_path) == path));
        }
        assert!(manifest.tombstones.is_empty());
        assert_eq!(
            fs::read(f.source.join("user/save/progress.sav")).unwrap(),
            b"original-save"
        );
        let plan = prepare_restore(
            &storage,
            &key,
            &f.agent.config.paths,
            &f.app,
            manifest.bundle_id,
            RestoreMode::CompleteApplication,
        )
        .await
        .unwrap();
        let index = read_pack_index(&storage, &key, &f.app, manifest.bundle_id)
            .await
            .unwrap();
        let roots = f.agent.roots.lock().clone();
        let home = f.agent.config.home.clone();
        fs::rename(&home, f.root.join("pre-steam-same-home-restore")).unwrap();
        directory(&home);
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
        assert_eq!(
            fs::read(&save).unwrap(),
            b"current Steam runtime workspace save"
        );
        assert_eq!(
            fs::read(&ordinary_settings).unwrap(),
            b"this Steam application's settings"
        );
        assert_eq!(
            fs::read(&compat_settings).unwrap(),
            b"this Steam application's settings"
        );
        assert_eq!(
            fs::read(&userdata).unwrap(),
            b"this Steam application's settings"
        );
        assert_eq!(proof.exists(), migrated);
        assert!(!f.source.join("user/save/progress.sav").exists());
        for path in &excluded {
            assert!(!path.exists(), "restored {}", path.display());
        }
        let restored = noland_discovery::runtime_backup_binding(&home, &identity)
            .unwrap()
            .unwrap();
        assert!(restored.permits(&save));
        assert!(restored.control_files.contains(&f.record));
        assert!(restored.control_files.contains(&f.receipt));
        assert_eq!(restored.control_files.contains(&proof), migrated);
        assert!(fs::metadata(f.view.join("hookshot")).unwrap().mode() & 0o100 != 0);
        let registered =
            noland_discovery::registered_runtime_identities(&home, &[identity]).unwrap();
        assert_eq!(registered.len(), 1);
        assert_eq!(registered[0].app_id, AppId::steam(480));
    }
}

#[tokio::test]
async fn full_native_workspace_captures_one_payload_tree_and_keeps_independent_state() {
    let f = Fixture::new(true, false);
    const FILES: usize = 1_000;
    for index in 0..FILES {
        let relative = format!("payload/asset-{index:04}.bin");
        let bytes = format!("copied asset {index}");
        write(&f.source.join(&relative), bytes.as_bytes());
        write(&f.view.join(&relative), bytes.as_bytes());
    }
    write(
        &f.source.join("payload/asset-0000.bin"),
        b"later original bytes",
    );
    write(
        &f.view.join("payload/asset-0000.bin"),
        b"active workspace bytes",
    );
    fs::remove_file(f.view.join("cache.bin")).unwrap();
    // Stored original ownership is audit evidence, not a reason to reopen the
    // superseded tree or resurrect a current-workspace deletion.
    let historical = f.source.join("user/save/progress.sav");
    f.index_historical_path(&historical, Some("file"));
    let ancestors = f.index_legacy_ancestors();
    let sibling = ancestors[1].join("SiblingGame/progress.sav");
    write(&sibling, b"another application's save");
    let independent = f.agent.config.home.join("Documents/independent-state.sav");
    write(&independent, b"explicit non-mirrored state");
    f.index_path(
        &independent,
        Some("file"),
        EvidenceKind::ExplicitUserBinding,
    );
    let storage = LocalStorage::new(f.root.join("cloud"));
    let key = MasterKey::generate();
    let (manifest, operation) = f.capture_with_operation(&storage, &key).await.unwrap();
    let anchor = f.source.join("hookshot");
    let original_files = manifest
        .files
        .iter()
        .filter_map(|file| file.source_path_hint.as_deref())
        .map(Path::new)
        .filter(|path| path.starts_with(&f.source))
        .collect::<Vec<_>>();
    assert_eq!(original_files, [anchor.as_path()]);
    assert_eq!(
        manifest
            .files
            .iter()
            .filter(|file| file
                .source_path_hint
                .as_deref()
                .is_some_and(|path| { Path::new(path).starts_with(f.view.join("payload")) }))
            .count(),
        FILES
    );
    assert!(
        manifest.files.len() < FILES + 64,
        "the capture must contain N workspace files, not N originals plus N mirrors"
    );
    let metrics = f
        .agent
        .db
        .get_operation_metrics(operation)
        .unwrap()
        .unwrap();
    assert!(
        metrics.num_candidate_paths < (FILES + 64) as u64,
        "{metrics:?}"
    );
    assert!(
        metrics.num_files_scanned < (FILES + 64) as u64,
        "{metrics:?}"
    );
    assert_eq!(
        file(&manifest, &f.view.join("payload/asset-0000.bin")).file_hash,
        noland_cas::blake3_hex(b"active workspace bytes")
    );
    assert_eq!(
        file(&manifest, &f.view.join("user/save/progress.sav")).file_hash,
        noland_cas::blake3_hex(b"private-save-v1")
    );
    assert!(has_file(&manifest, &independent));
    assert!(has_file(
        &manifest,
        &f.private_home.join(".config/game/settings.ini")
    ));
    assert!(has_file(&manifest, &f.record));
    assert!(has_file(&manifest, &f.receipt));
    assert!(!has_file(&manifest, &f.view.join("cache.bin")));
    assert!(!has_file(&manifest, &f.source.join("cache.bin")));
    assert!(!has_file(&manifest, &sibling));
    assert!(!manifest.files.iter().any(|file| file
        .source_path_hint
        .as_deref()
        .is_some_and(|path| path.contains("/app-data/other/"))));
    assert!(manifest.tombstones.is_empty());
    let history = f.agent.db.associations_for_app(&f.app).unwrap();
    let (original, _) = history
        .iter()
        .find(|(record, _)| Path::new(&record.canonical_path) == historical)
        .unwrap();
    assert_eq!(original.last_scanned_at, Some(1));
    for index in 0..FILES {
        assert!(f
            .source
            .join(format!("payload/asset-{index:04}.bin"))
            .is_file());
    }
    assert_eq!(fs::read(&historical).unwrap(), b"original-save");
    assert_eq!(
        fs::read(f.source.join("cache.bin")).unwrap(),
        b"original-cache"
    );
    assert_eq!(fs::read(&sibling).unwrap(), b"another application's save");
    assert_eq!(
        fs::read(f.source.join("payload/asset-0000.bin")).unwrap(),
        b"later original bytes"
    );
}

#[tokio::test]
async fn native_workspace_prunes_inherited_originals_without_tombstones_and_restores_same_home() {
    let f = Fixture::new(true, false);
    let registration = fs::read(&f.record).unwrap();
    let mut read_only: Value = serde_json::from_slice(&registration).unwrap();
    read_only["writableApp"] = json!(false);
    write(&f.record, &serde_json::to_vec(&read_only).unwrap());
    let independent = f.agent.config.home.join("Documents/independent-state.sav");
    write(&independent, b"explicit non-mirrored state");
    f.index_path(
        &independent,
        Some("file"),
        EvidenceKind::ExplicitUserBinding,
    );
    let storage = LocalStorage::new(f.root.join("cloud"));
    let key = MasterKey::generate();
    let parent = f.capture(&storage, &key).await.unwrap();
    for relative in [
        "romfs/asset.bin",
        "user/save/progress.sav",
        "cache.bin",
        "archives/game.zip",
        "backup/archive.nup",
        "mods/custom.bin",
    ] {
        assert!(has_file(&parent, &f.source.join(relative)));
    }
    write(&f.record, &registration);
    let current_save = f.view.join("user/save/progress.sav");
    write(&current_save, b"newer authoritative workspace save");
    fs::remove_file(f.view.join("cache.bin")).unwrap();
    f.agent
        .db
        .append_app_mutation(&AppMutationRecord::new(
            f.app.clone(),
            f.source.join("cache.bin").to_string_lossy(),
            AppMutationKind::Delete,
        ))
        .unwrap();
    let mut renamed = AppMutationRecord::new(
        f.app.clone(),
        f.source.join("mods/renamed.bin").to_string_lossy(),
        AppMutationKind::Rename,
    );
    renamed.previous_path = Some(
        f.source
            .join("mods/custom.bin")
            .to_string_lossy()
            .into_owned(),
    );
    f.agent.db.append_app_mutation(&renamed).unwrap();
    let before = f.agent.db.get_app(&f.app).unwrap().unwrap();
    let next = f.capture(&storage, &key).await.unwrap();
    assert_eq!(next.parent_bundle_id, Some(parent.bundle_id));
    assert_eq!(next.app.app_id, before.app_id);
    let anchor = f.source.join("hookshot");
    assert!(next.files.iter().all(|file| {
        file.source_path_hint
            .as_deref()
            .is_none_or(|path| !Path::new(path).starts_with(&f.source) || Path::new(path) == anchor)
    }));
    assert!(has_file(&next, &anchor));
    assert!(has_file(&next, &independent));
    assert_eq!(
        file(&next, &current_save).file_hash,
        noland_cas::blake3_hex(b"newer authoritative workspace save")
    );
    assert_eq!(
        file(&next, &f.view.join("romfs/asset.bin")).file_hash,
        noland_cas::blake3_hex(b"private workspace asset")
    );
    assert!(!has_file(&next, &f.view.join("cache.bin")));
    assert!(
        next.tombstones.is_empty(),
        "scope reduction is not a restore deletion"
    );
    for relative in [
        "romfs/asset.bin",
        "user/save/progress.sav",
        "cache.bin",
        "archives/game.zip",
        "backup/archive.nup",
        "mods/custom.bin",
    ] {
        let path = f.source.join(relative);
        assert!(path.is_file(), "original data must stay local");
        assert!(f
            .agent
            .db
            .associations_for_app(&f.app)
            .unwrap()
            .iter()
            .any(|(record, _)| Path::new(&record.canonical_path) == path));
    }
    assert_eq!(
        fs::read(f.source.join("user/save/progress.sav")).unwrap(),
        b"original-save"
    );
    assert_eq!(
        fs::read(f.source.join("cache.bin")).unwrap(),
        b"original-cache"
    );
    let plan = prepare_restore(
        &storage,
        &key,
        &f.agent.config.paths,
        &f.app,
        next.bundle_id,
        RestoreMode::CompleteApplication,
    )
    .await
    .unwrap();
    let index = read_pack_index(&storage, &key, &f.app, next.bundle_id)
        .await
        .unwrap();
    let home = f.agent.config.home.clone();
    fs::rename(&home, f.root.join("before-same-home-restore")).unwrap();
    directory(&home);
    restore_pipelined(
        &storage,
        &key,
        &plan,
        &index,
        &LogicalRootMap::from_home(&home),
        None,
        None,
        PipelineOptions::default(),
        |_| Ok(()),
    )
    .await
    .unwrap();
    assert_eq!(
        fs::read(&current_save).unwrap(),
        b"newer authoritative workspace save"
    );
    assert_eq!(
        fs::read(&independent).unwrap(),
        b"explicit non-mirrored state"
    );
    assert_eq!(
        fs::read(f.view.join("archives/game.zip")).unwrap(),
        b"preserved original ZIP"
    );
    let restored_source = fs::read_dir(&f.source)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(restored_source, [std::ffi::OsString::from("hookshot")]);
    assert!(fs::metadata(f.view.join("hookshot")).unwrap().mode() & 0o100 != 0);
    assert_eq!(
        fs::read(f.view.join("hookshot")).unwrap(),
        b"private updated executable"
    );
    let binding = noland_discovery::runtime_backup_binding(&home, &before)
        .unwrap()
        .unwrap();
    assert!(binding.permits(&current_save));
    assert!(binding.is_superseded_source(&f.source.join("romfs/asset.bin")));
    assert!(!binding.is_superseded_source(&anchor));
    let discovered = noland_discovery::registered_runtime_identities(&home, &[before]).unwrap();
    assert_eq!(discovered.len(), 1);
    assert_eq!(discovered[0].app_id, f.app);
}

#[test]
fn every_reconciliation_mode_prunes_native_originals_without_losing_the_elf_anchor() {
    let reconcile: [fn(&StateAgent, &AppId) -> Result<usize>; 4] = [
        reconcile_app_for_complete_backup,
        reconcile_app_for_refresh,
        reconcile_app,
        reconcile_install_roots,
    ];
    for reconcile in reconcile {
        let f = Fixture::new(true, false);
        reconcile(&f.agent, &f.app).unwrap();
        assert!(f
            .agent
            .db
            .get_path_by_canonical(f.source.join("hookshot").to_str().unwrap())
            .unwrap()
            .is_some());
        assert!(f
            .agent
            .db
            .get_path_by_canonical(f.source.join("romfs/asset.bin").to_str().unwrap())
            .unwrap()
            .is_none());
        assert_eq!(
            f.agent.db.known_roots(Some(&f.app)).unwrap().len(),
            1,
            "the direct anchor must not be persisted as traversal authority"
        );
    }
}

#[tokio::test]
async fn native_runtime_migration_reconciles_history_without_reintroducing_old_layout() {
    let mut f = Fixture::new(false, false);
    let storage = LocalStorage::new(f.root.join("cloud"));
    let key = MasterKey::generate();
    let parent = f.capture(&storage, &key).await.unwrap();
    let old_save = f.view.join("user/save/progress.sav");
    assert!(parent
        .files
        .iter()
        .any(|entry| entry.source_path_hint.as_deref() == old_save.to_str()));
    let retired = f.migrate_to_workspace();
    let new_save = f.view.join("user/save/progress.sav");
    write(&new_save, b"current workspace progress after migration");
    let identity_before = f.agent.db.get_app(&f.app).unwrap().unwrap();
    let next = f.capture(&storage, &key).await.unwrap();
    assert_eq!(next.parent_bundle_id, Some(parent.bundle_id));
    for path in &retired {
        assert!(!next
            .files
            .iter()
            .any(|entry| entry.source_path_hint.as_deref() == path.to_str()));
        assert!(
            !next.tombstones.iter().any(|entry| {
                f.agent.roots.lock().classify(path).is_some_and(|logical| {
                    entry.logical_root == logical.logical_root.as_token()
                        && entry.relative_path == logical.relative_path
                })
            }),
            "retirement is not a restore deletion instruction"
        );
        assert!(path.exists(), "retirement must not delete local data");
    }
    assert_eq!(fs::read(&old_save).unwrap(), b"private-save-v1");
    assert!(f
        .agent
        .db
        .associations_for_app(&f.app)
        .unwrap()
        .iter()
        .any(|(record, _)| Path::new(&record.canonical_path) == old_save));
    assert_eq!(
        f.agent.db.get_app(&f.app).unwrap().unwrap().app_id,
        identity_before.app_id
    );
    assert!(next
        .files
        .iter()
        .any(|entry| entry.source_path_hint.as_deref() == new_save.to_str()));
    assert!(next
        .files
        .iter()
        .any(|entry| entry.source_path_hint.as_deref() == f.receipt.to_str()));
    assert!(next
        .files
        .iter()
        .any(|entry| entry.source_path_hint.as_deref()
            == f.receipt
                .parent()
                .unwrap()
                .join("legacy-import.json")
                .to_str()));

    // A fresh same-home restore must recreate only the current registration and
    // workspace. It must not need the retired view or its old filesystem IDs.
    let plan = prepare_restore(
        &storage,
        &key,
        &f.agent.config.paths,
        &f.app,
        next.bundle_id,
        RestoreMode::CompleteApplication,
    )
    .await
    .unwrap();
    let index = read_pack_index(&storage, &key, &f.app, next.bundle_id)
        .await
        .unwrap();
    let home = f.agent.config.home.clone();
    fs::rename(&home, f.root.join("pre-restore-home")).unwrap();
    directory(&home);
    restore_pipelined(
        &storage,
        &key,
        &plan,
        &index,
        &LogicalRootMap::from_home(&home),
        None,
        None,
        PipelineOptions::default(),
        |_| Ok(()),
    )
    .await
    .unwrap();
    assert_eq!(
        fs::read(&new_save).unwrap(),
        b"current workspace progress after migration"
    );
    assert!(!old_save.exists());
    assert!(!home
        .join(".local/share/noland-runtime/portable/current/manifest.json")
        .exists());
    let current = noland_discovery::runtime_backup_binding(&home, &identity_before)
        .unwrap()
        .unwrap();
    assert!(current.permits(&new_save));
    assert!(current.is_retired(&old_save));
    let registered =
        noland_discovery::registered_runtime_identities(&home, &[identity_before]).unwrap();
    assert_eq!(registered.len(), 1);
    assert_eq!(registered[0].app_id, f.app);
}

#[tokio::test]
async fn migration_does_not_retire_unproved_or_later_changed_legacy_payload() {
    for case in ["changed", "new-unproved", "other-app", "missing-proof"] {
        let mut f = Fixture::new(false, false);
        let storage = LocalStorage::new(f.root.join("cloud"));
        let key = MasterKey::generate();
        let parent = f.capture(&storage, &key).await.unwrap();
        let retired = f.migrate_to_workspace();
        let old_view = f
            .agent
            .config
            .home
            .join(".local/share/noland-runtime/portable/current/view");
        match case {
            "changed" => write(
                &old_view.join("user/save/progress.sav"),
                b"unmigrated later save",
            ),
            "new-unproved" | "other-app" => {
                let path = if case == "other-app" {
                    f.agent
                        .config
                        .home
                        .join(".local/share/noland-runtime/portable/other/view/save")
                } else {
                    old_view.join("unproved-index-key")
                };
                // No creation under the pinned old root: this tests that even
                // an unproved historical key cannot borrow retirement scope.
                f.index_historical_path(&path, Some("file"));
            }
            "missing-proof" => fs::rename(
                f.receipt.parent().unwrap().join("legacy-import.json"),
                f.root.join("withheld-proof.json"),
            )
            .unwrap(),
            _ => unreachable!(),
        }
        assert!(f.capture(&storage, &key).await.is_err(), "{case}");
        assert_eq!(
            f.agent.db.latest_commit(&f.app).unwrap().unwrap().1,
            parent.bundle_id
        );
        assert!(retired.iter().all(|path| path.exists()));
        assert_eq!(
            fs::read(f.view.join("user/save/progress.sav")).unwrap(),
            b"private-save-v1"
        );
    }
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
            f.source.join("hookshot"),
        ] {
            file(&first, &path);
        }
        let authoritative = if workspace && !appimage {
            &f.view
        } else {
            &f.source
        };
        for relative in ["archives/game.zip", "backup/archive.nup", "mods/custom.bin"] {
            file(&first, &authoritative.join(relative));
        }
        if workspace && !appimage {
            assert!(first.files.iter().all(|file| {
                file.source_path_hint.as_deref().is_none_or(|path| {
                    !Path::new(path).starts_with(&f.source)
                        || Path::new(path) == f.source.join("hookshot")
                })
            }));
            assert_eq!(
                file(&first, &f.view.join("romfs/asset.bin")).file_hash,
                noland_cas::blake3_hex(b"private workspace asset")
            );
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
        if workspace && !appimage {
            assert!(!destination(&f.source.join("user/save/progress.sav")).exists());
            assert!(!destination(&f.source.join("romfs/asset.bin")).exists());
            assert_eq!(
                fs::read(destination(&f.view.join("archives/game.zip"))).unwrap(),
                b"preserved original ZIP"
            );
        } else {
            assert_eq!(
                fs::read(destination(&f.source.join("user/save/progress.sav"))).unwrap(),
                b"original-save"
            );
        }
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
