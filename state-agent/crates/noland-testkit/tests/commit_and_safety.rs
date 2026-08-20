use bytes::Bytes;
use noland_crypto::{derive_keys, wrap_envelope, MasterKey};
use noland_state_core::*;
use noland_state_core::COMMITTED_MARKER;
use noland_storage::{forbid_rclone_sync, LocalStorage, RemoteKey, SharedStorageProvider};
use uuid::Uuid;

#[tokio::test]
async fn incomplete_bundle_without_committed_is_invisible() {
    let root = std::env::temp_dir().join(format!("noland-commit-{}", Uuid::new_v4()));
    let storage = LocalStorage::new(&root);
    storage.ensure_root().await.unwrap();
    let app = AppId::steam(9);
    let bundle = Uuid::new_v4();
    let prefix = bundle_dir(&app, bundle);
    storage
        .put_small_versioned(Bytes::from("not-a-manifest"), &RemoteKey::new(format!("{prefix}/manifest.enc")))
        .await
        .unwrap();
    assert!(storage
        .stat(&RemoteKey::new(format!("{prefix}/{COMMITTED_MARKER}")))
        .await
        .unwrap()
        .is_none());
    let master = MasterKey::generate();
    let err = noland_storage::read_committed_manifest(&storage, &master, &app, bundle).await;
    assert!(matches!(err, Err(StateError::IncompleteCommit)));
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn committed_catalog_only_after_marker() {
    let root = std::env::temp_dir().join(format!("noland-cat-{}", Uuid::new_v4()));
    let storage = LocalStorage::new(&root);
    storage.ensure_root().await.unwrap();
    let master = MasterKey::generate();
    let keys = derive_keys(&master);
    let mut manifest = BundleManifest::new(
        ManifestApp {
            app_id: AppId::steam(480),
            display_name: "Game".into(),
            aliases: vec!["Game Alias".into()],
            desktop_entry_id: Some("steam-480.desktop".into()),
            steam_app_id: Some(480),
            launcher: Some(LauncherKind::Steam),
            canonical_executable: Some("/usr/bin/game".into()),
            icon_path: Some("/usr/share/icons/game.png".into()),
        },
        ManifestSource {
            instance_id: Uuid::new_v4(),
            image_id: "img".into(),
            captured_at: chrono::Utc::now(),
        },
        BackupMode::PersonalState,
    );
    manifest.files.push(ManifestFile {
        logical_root: "$XDG_DATA_HOME".into(),
        relative_path: "game/save.dat".into(),
        source_path_hint: None,
        file_type: "file".into(),
        size: 4,
        file_hash: "blake3:00".into(),
        chunks: vec![],
        mode: None,
        mtime_ns: None,
        uid: None,
        gid: None,
        symlink_target: None,
        persistence_class: PersistenceClass::PersistentState,
        semantic_role: SemanticRole::UserState,
        association_confidence: 0.95,
        shared_app_ids: vec![],
    });
    let prefix = bundle_dir(&manifest.app.app_id, manifest.bundle_id);
    let enc = wrap_envelope(&keys.manifest, b"manifest", &serde_json::to_vec(&manifest).unwrap()).unwrap();
    storage
        .put_small_versioned(Bytes::from(enc), &RemoteKey::new(format!("{prefix}/manifest.enc")))
        .await
        .unwrap();
    let before = noland_storage::update_catalog_with_bundle(&storage, &master, &manifest, 0).await;
    assert!(matches!(before, Err(StateError::IncompleteCommit)));
    storage
        .put_small_versioned(
            Bytes::from(manifest.commit_id.to_string()),
            &RemoteKey::new(format!("{prefix}/{COMMITTED_MARKER}")),
        )
        .await
        .unwrap();
    let catalog = noland_storage::update_catalog_with_bundle(&storage, &master, &manifest, 12)
        .await
        .unwrap();
    assert_eq!(catalog.apps.len(), 1);
    let app = &catalog.apps[0];
    assert_eq!(app.aliases, vec!["Game Alias"]);
    assert_eq!(app.desktop_entry_id.as_deref(), Some("steam-480.desktop"));
    assert_eq!(app.steam_app_id, Some(480));
    assert_eq!(app.launcher, Some(LauncherKind::Steam));
    assert_eq!(
        app.canonical_executable.as_deref(),
        Some(std::path::Path::new("/usr/bin/game"))
    );
    assert_eq!(
        app.icon_path.as_deref(),
        Some(std::path::Path::new("/usr/share/icons/game.png"))
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn rclone_sync_is_rejected() {
    assert!(forbid_rclone_sync(&["copy".into()]).is_ok());
    assert!(forbid_rclone_sync(&["sync".into(), "a".into(), "b".into()]).is_err());
}

#[test]
fn adapter_session_is_provider_agnostic_and_ephemeral() {
    use noland_rclone_adapter::{
        session_from_input, AdapterCredential, AdapterInput, ProviderKind, TokenMode,
    };
    use std::collections::BTreeMap;

    let drive = AdapterInput {
        provider: ProviderKind::GoogleDrive,
        remote_name: "noland_drive".into(),
        credentials: AdapterCredential::OAuth2 {
            access_token: "access".into(),
            refresh_token: Some("refresh-secret".into()),
            expires_at: 1_800_000_000,
        },
        fields: BTreeMap::from([("folder".into(), "Noland Shared Storage".into())]),
        bucket: None,
        prefix: None,
    };
    let session = session_from_input(&drive, "op", TokenMode::Ephemeral).unwrap();
    assert_eq!(session.backend_type, "drive");
    assert!(!session.config_ini.contains("refresh-secret"));
    let storage = noland_storage::RcloneStorage::from_session(
        &session,
        std::path::Path::new("/run/noland/storage/op/rclone.conf"),
    );
    assert_eq!(storage.provider_label(), "rclone:drive");

    let b2 = AdapterInput {
        provider: ProviderKind::BackblazeB2,
        remote_name: "noland_b2".into(),
        credentials: AdapterCredential::BackblazeB2 {
            key_id: "id".into(),
            application_key: "key".into(),
        },
        fields: BTreeMap::new(),
        bucket: Some("bucket".into()),
        prefix: Some("noland".into()),
    };
    let b2_session = session_from_input(&b2, "op2", TokenMode::Ephemeral).unwrap();
    assert_eq!(b2_session.backend_type, "b2");
    assert_eq!(b2_session.root, "bucket/noland");
}

#[test]
fn path_traversal_and_symlink_escape_are_rejected() {
    assert!(validate_relative_path("../etc/passwd").is_err());
    assert!(validate_relative_path("saves/../../etc/passwd").is_err());
    assert!(validate_relative_path("/absolute").is_err());
    assert!(validate_relative_path("saves/world.dat").is_ok());
    let root = std::env::temp_dir().join(format!("noland-safe-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    assert!(join_validated(&root, "../escape").is_err());
    assert!(join_validated(&root, "ok/file").is_ok());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn seal_blocks_automatic_delete_until_sealed() {
    let db = noland_state_db::StateDb::open_in_memory().unwrap();
    let app = AppIdentity::new(AppId::desktop("game"), "Game");
    db.upsert_app(&app).unwrap();
    db.mark_dirty(&app.app_id, None, false).unwrap();
    assert!(!db.list_dirty_apps().unwrap().is_empty());
    assert!(db.latest_seal().unwrap().is_none());
}
