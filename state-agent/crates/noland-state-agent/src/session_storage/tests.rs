use super::*;
use crate::index_test_support::{root, session, write_catalog, TestProvider};
use crate::AgentConfig;

#[tokio::test]
async fn immutable_catalog_reuse_rebuilds_corruption_and_observes_external_changes_and_removal() {
    let root = root("catalog-cache");
    let agent = StateAgent::boot(AgentConfig::isolated(root.join("agent"))).unwrap();
    let provider = TestProvider::new(root.join("cloud"));
    let master = MasterKey::generate();
    let session = session(&provider.inner.root);
    let catalog = CatalogDocument::empty();
    write_catalog(&provider, &master, &catalog).await;
    let storage = SessionStorage::new(provider.clone(), &agent, &session, &master);
    let key = format!("catalog/commits/{}.enc", catalog.catalog_commit_id);
    for _ in 0..2 {
        let loaded = noland_storage::load_catalog(&storage, &master)
            .await
            .unwrap();
        assert_eq!(loaded.catalog_commit_id, catalog.catalog_commit_id);
    }
    assert_eq!(
        provider.reads(&key),
        1,
        "only immutable ciphertext is reused"
    );
    assert_eq!(
        provider.reads("catalog/LATEST"),
        2,
        "mutable pointer always fresh"
    );
    let binding = crate::storage_scope::binding(storage.storage_identity(), &master).unwrap();
    let mut corrupt = agent.db.cached_metadata(&binding, &key).unwrap().unwrap();
    corrupt.payload[0] ^= 1;
    agent
        .db
        .cache_authenticated_metadata(&binding, &key, &corrupt.payload, &corrupt.receipt)
        .unwrap();
    noland_storage::load_catalog(&storage, &master)
        .await
        .unwrap();
    assert_eq!(
        provider.reads(&key),
        2,
        "bad local receipt rebuilds from remote"
    );
    let external = CatalogDocument::empty();
    write_catalog(&provider, &master, &external).await;
    assert_eq!(
        noland_storage::load_catalog(&storage, &master)
            .await
            .unwrap()
            .catalog_commit_id,
        external.catalog_commit_id
    );
    let removed = format!("catalog/commits/{}.enc", external.catalog_commit_id);
    // Simulate an external loss of our fixture object. Production deletion
    // deliberately cannot remove immutable catalog history through this API.
    std::fs::remove_file(provider.inner.root.join(&removed)).unwrap();
    assert!(
        noland_storage::load_catalog(&storage, &master)
            .await
            .is_err(),
        "cached bytes cannot conceal removed committed data"
    );
    crate::cloud_catalog::invalidate(&agent, storage.storage_identity(), &master);
    assert!(agent.db.cached_metadata(&binding, &key).unwrap().is_none());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn catalog_cache_never_crosses_account_endpoint_or_key_and_never_caches_mutable_reads() {
    let root = root("catalog-scope");
    let agent = StateAgent::boot(AgentConfig::isolated(root.join("agent"))).unwrap();
    let provider = TestProvider::new(root.join("cloud"));
    let master = MasterKey::generate();
    let mut session = session(&provider.inner.root);
    session.provider = "sftp".into();
    session.backend_type = "sftp".into();
    session.config_ini =
        "[fixture]\ntype = sftp\nhost = one.test\nuser = alice\npass = first\n".into();
    let catalog = CatalogDocument::empty();
    write_catalog(&provider, &master, &catalog).await;
    let key = RemoteKey::new(format!("catalog/commits/{}.enc", catalog.catalog_commit_id));
    let storage = SessionStorage::new(provider.clone(), &agent, &session, &master);
    storage
        .read_small(&key, MAX_SMALL_READ_BYTES)
        .await
        .unwrap();
    assert_eq!(provider.reads(key.as_str()), 1);
    for config in [
        session.config_ini.replace("alice", "bob"),
        session.config_ini.replace("one.test", "two.test"),
    ] {
        let mut changed = session.clone();
        changed.config_ini = config;
        let storage = SessionStorage::new(provider.clone(), &agent, &changed, &master);
        storage
            .read_small(&key, MAX_SMALL_READ_BYTES)
            .await
            .unwrap();
        assert_eq!(storage.metadata_metrics()["immutable_hits"], 0);
    }
    session.config_ini = session.config_ini.replace("first", "rotated");
    let rotated = SessionStorage::new(provider.clone(), &agent, &session, &master);
    rotated
        .read_small(&key, MAX_SMALL_READ_BYTES)
        .await
        .unwrap();
    assert_eq!(
        rotated.metadata_metrics()["immutable_hits"],
        1,
        "credential rotation is not namespace rotation"
    );
    let wrong_key = MasterKey::generate();
    let wrong = SessionStorage::new(provider.clone(), &agent, &session, &wrong_key);
    assert!(wrong.read_small(&key, MAX_SMALL_READ_BYTES).await.is_err());
    for _ in 0..2 {
        rotated
            .read_small(&RemoteKey::new("catalog/LATEST"), 1024)
            .await
            .unwrap();
    }
    assert_eq!(provider.reads("catalog/LATEST"), 2);
    assert!(
        rotated.read_small(&key, 1).await.is_err(),
        "cache obeys the caller's read bound"
    );
    provider.controls.fail_stats.store(true, Ordering::Relaxed);
    assert!(rotated
        .read_small(&key, MAX_SMALL_READ_BYTES)
        .await
        .is_err());
    provider.controls.fail_stats.store(false, Ordering::Relaxed);
    assert!(rotated.read_small(&key, MAX_SMALL_READ_BYTES).await.is_ok());
    std::fs::remove_dir_all(root).unwrap();
}
