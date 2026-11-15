use super::*;
use crate::index_test_support::{root, session, TestProvider};
use crate::{session_storage::SessionStorage, AgentConfig};
use noland_storage::{commit_bundle_with_index, RemoteKey};

async fn fixture(label: &str) -> (PathBuf, StateAgent, TestProvider, MasterKey, BundleManifest) {
    let root = root(label);
    let agent = StateAgent::boot(AgentConfig::isolated(root.join("agent"))).unwrap();
    let provider = TestProvider::new(root.join("cloud"));
    let master = MasterKey::generate();
    let manifest = BundleManifest::new(
        ManifestApp::from(&AppIdentity::new(
            AppId::desktop("cache-fixture"),
            "Cache fixture",
        )),
        ManifestSource {
            instance_id: Uuid::new_v4(),
            image_id: "fixture".into(),
            captured_at: Utc::now(),
        },
        BackupMode::CompleteApplication,
    );
    commit_bundle_with_index(&provider, &master, &manifest, &[], Some(b"[]"), None)
        .await
        .unwrap();
    (root, agent, provider, master, manifest)
}

#[tokio::test]
async fn restore_reuses_authenticated_metadata_but_checks_current_commit_and_rebuilds_corruption() {
    let (root, agent, provider, master, manifest) = fixture("restore-cache").await;
    let app = &manifest.app.app_id;
    let bundle = manifest.bundle_id;
    let prefix = bundle_dir(app, bundle);
    let operation = Uuid::new_v4();
    let first = read_restore_metadata(&agent, &provider, &master, app, bundle, operation)
        .await
        .unwrap();
    assert_eq!(first.2["immutable_metadata_reused"], false);
    let index_key = format!("{prefix}/index.enc");
    agent
        .db
        .fail_sync_journal_item(operation, &index_key, "interrupted attempt")
        .unwrap();
    let second = read_restore_metadata(&agent, &provider, &master, app, bundle, operation)
        .await
        .unwrap();
    assert_eq!(second.2["immutable_metadata_reused"], true);
    assert_eq!(
        agent
            .db
            .get_sync_journal_entry(operation, &index_key)
            .unwrap()
            .unwrap()
            .state,
        SyncJournalState::Completed
    );
    assert_eq!(provider.reads(&format!("{prefix}/manifest.enc")), 1);
    assert_eq!(provider.reads(&format!("{prefix}/index.enc")), 1);
    let binding = repository_cache_binding(provider.storage_identity(), &master).unwrap();
    std::fs::write(
        parent_cache_dir(&agent, app, bundle)
            .join(binding)
            .join("index.json"),
        b"corrupt",
    )
    .unwrap();
    let repaired = read_restore_metadata(&agent, &provider, &master, app, bundle, operation)
        .await
        .unwrap();
    assert_eq!(repaired.2["immutable_metadata_reused"], false);
    assert_eq!(provider.reads(&format!("{prefix}/index.enc")), 2);
    provider
        .controls
        .fail_stats
        .store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(
        read_restore_metadata(&agent, &provider, &master, app, bundle, operation)
            .await
            .is_err()
    );
    provider
        .controls
        .fail_stats
        .store(false, std::sync::atomic::Ordering::Relaxed);
    let marker = RemoteKey::new(format!("{prefix}/{COMMITTED_MARKER}"));
    provider.delete_object(&marker).await.unwrap();
    assert!(matches!(
        read_restore_metadata(&agent, &provider, &master, app, bundle, operation).await,
        Err(StateError::IncompleteCommit)
    ));
    provider
        .put_small_versioned(bytes::Bytes::from_static(b"COMMITTED"), &marker)
        .await
        .unwrap();
    provider
        .delete_object(&RemoteKey::new(format!("{prefix}/index.enc")))
        .await
        .unwrap();
    assert!(
        read_restore_metadata(&agent, &provider, &master, app, bundle, operation)
            .await
            .is_err(),
        "cached index cannot stand in for removed remote metadata"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn deleted_bundle_and_transport_failure_are_not_hidden_by_authenticated_metadata() {
    let (root, agent, provider, master, manifest) = fixture("restore-deleted").await;
    let app = &manifest.app.app_id;
    let bundle = manifest.bundle_id;
    let operation = Uuid::new_v4();
    read_restore_metadata(&agent, &provider, &master, app, bundle, operation)
        .await
        .unwrap();
    provider
        .controls
        .fail_reads
        .store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(
        read_restore_metadata(&agent, &provider, &master, app, bundle, operation)
            .await
            .is_err()
    );
    provider
        .controls
        .fail_reads
        .store(false, std::sync::atomic::Ordering::Relaxed);
    assert!(
        read_restore_metadata(&agent, &provider, &master, app, bundle, operation)
            .await
            .is_ok()
    );
    noland_storage::delete_bundle(
        &provider,
        &master,
        &noland_storage::BundleDeletion {
            app_id: app.clone(),
            bundle_id: bundle,
            commit_id: manifest.commit_id,
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        read_restore_metadata(&agent, &provider, &master, app, bundle, operation).await,
        Err(StateError::NotFound(_))
    ));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn restore_cache_is_account_endpoint_root_and_key_bound() {
    let (root, agent, provider, master, manifest) = fixture("restore-scope").await;
    let mut session = session(&provider.inner.root);
    session.provider = "sftp".into();
    session.backend_type = "sftp".into();
    session.config_ini =
        "[fixture]\ntype = sftp\nhost = server\nuser = alice\npass = ignored\n".into();
    let app = &manifest.app.app_id;
    let bundle = manifest.bundle_id;
    let operation = Uuid::new_v4();
    let storage = SessionStorage::new(provider.clone(), &agent, &session, &master);
    read_restore_metadata(&agent, &storage, &master, app, bundle, operation)
        .await
        .unwrap();
    assert!(
        read_restore_metadata(&agent, &storage, &master, app, bundle, operation)
            .await
            .unwrap()
            .2["immutable_metadata_reused"]
            .as_bool()
            .unwrap()
    );
    let mut account = session.clone();
    account.config_ini = account.config_ini.replace("alice", "bob");
    let mut endpoint = session.clone();
    endpoint.config_ini = endpoint
        .config_ini
        .replace("host = server", "host = another");
    let mut other_root = session.clone();
    other_root.root = "/other-repo".into();
    for changed in [account, endpoint, other_root] {
        let storage = SessionStorage::new(provider.clone(), &agent, &changed, &master);
        let loaded = read_restore_metadata(&agent, &storage, &master, app, bundle, operation)
            .await
            .unwrap();
        assert_eq!(loaded.2["immutable_metadata_reused"], false);
    }
    let wrong_key = MasterKey::generate();
    let storage = SessionStorage::new(provider.clone(), &agent, &session, &wrong_key);
    assert!(
        read_restore_metadata(&agent, &storage, &wrong_key, app, bundle, operation)
            .await
            .is_err()
    );
    assert_eq!(
        provider.reads(&format!("{}/manifest.enc", bundle_dir(app, bundle))),
        5
    );
    std::fs::remove_dir_all(root).unwrap();
}
