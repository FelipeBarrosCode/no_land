use super::*;

fn fixture(label: &str) -> (PathBuf, StateAgent, AppId, PathBuf, LocalStorage, MasterKey) {
    let root = std::env::current_dir()
        .unwrap()
        .join("target/backup-optimizations")
        .join(format!("{label}-{}", Uuid::new_v4()));
    let agent = StateAgent::boot(crate::AgentConfig::isolated(root.clone())).unwrap();
    let app = AppId::desktop("optimization-fixture");
    agent
        .db
        .upsert_app(&AppIdentity::new(app.clone(), "Optimization Fixture"))
        .unwrap();
    let install = agent.config.home.join("Games/Fixture");
    let path = install.join("custom-data/arbitrary.bin");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"immutable content").unwrap();
    agent
        .db
        .add_known_root(&app, "install", install.to_str().unwrap())
        .unwrap();
    let storage = LocalStorage::new(root.join("remote"));
    (root, agent, app, path, storage, MasterKey::generate())
}

async fn backup(
    agent: &StateAgent,
    app: &AppId,
    storage: &LocalStorage,
    key: &MasterKey,
) -> (BundleManifest, OperationMetrics) {
    let operation = Uuid::new_v4();
    let manifest = run_backup(
        agent,
        app,
        BackupMode::CompleteApplication,
        BackupPerformanceMode::Balanced,
        storage,
        key,
        Some(operation),
    )
    .await
    .unwrap();
    let metrics = operation_metrics(&agent.db.get_operation(operation).unwrap().unwrap());
    (manifest, metrics)
}

#[tokio::test]
async fn full_cloud_deletion_invalidates_cached_chunks_and_fresh_backup_reuploads_source_data() {
    let (root, agent, app, source, storage, key) = fixture("deleted-parent");
    let (original, _) = backup(&agent, &app, &storage, &key).await;
    let old_index = read_pack_index(&storage, &key, &app, original.bundle_id)
        .await
        .unwrap();
    let deletion = noland_storage::BundleDeletion {
        app_id: app.clone(),
        bundle_id: original.bundle_id,
        commit_id: original.commit_id,
    };
    noland_storage::delete_bundle(&storage, &key, &deletion)
        .await
        .unwrap();
    assert!(
        noland_storage::commit_bundle(&storage, &key, &original, &[], None)
            .await
            .is_err()
    );
    let (fresh, _) = backup(&agent, &app, &storage, &key).await;
    assert_ne!(fresh.bundle_id, original.bundle_id);
    assert!(fresh.parent_bundle_id.is_none());
    assert_eq!(fresh.files[0].file_hash, original.files[0].file_hash);
    let new_index = read_pack_index(&storage, &key, &app, fresh.bundle_id)
        .await
        .unwrap();
    assert!(
        !storage.root.join(pack_key(&old_index[0].pack_id)).exists(),
        "old ciphertext is physically deleted"
    );
    assert_ne!(
        old_index[0].pack_id, new_index[0].pack_id,
        "stale cache hints cannot reuse retired ciphertext"
    );
    assert_eq!(
        noland_pack::extract_chunk(
            &storage.root.join(pack_key(&new_index[0].pack_id)),
            &new_index[0],
            &key
        )
        .unwrap(),
        b"immutable content"
    );
    assert_eq!(std::fs::read(&source).unwrap(), b"immutable content");
    assert_eq!(
        noland_storage::load_catalog(&storage, &key)
            .await
            .unwrap()
            .apps[0]
            .bundles
            .len(),
        1
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn complete_steam_backup_resolves_launcher_alias_and_captures_canonical_userdata() {
    let root = std::env::current_dir().unwrap().join(format!(
        "target/backup-optimizations/steam-root-alias-{}",
        Uuid::new_v4()
    ));
    let agent = StateAgent::boot(crate::AgentConfig::isolated(root.clone())).unwrap();
    let steam = agent.config.home.join(".steam/debian-installation");
    let files = [
        (
            "steamapps/common/Helldivers 2/data/bundle.bin",
            b"game bytes".as_slice(),
        ),
        (
            "userdata/123/553850/remote/save.dat",
            b"saved progress".as_slice(),
        ),
        (
            "steamapps/shadercache/553850/pipeline.bin",
            b"shader bytes".as_slice(),
        ),
        (
            "steamapps/compatdata/553850/pfx/drive_c/users/steamuser/AppData/Local/save.dat",
            b"prefix save".as_slice(),
        ),
    ];
    for (relative, bytes) in files {
        let path = steam.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    let other_save = steam.join("userdata/123/999/remote/save.dat");
    std::fs::create_dir_all(other_save.parent().unwrap()).unwrap();
    std::fs::write(&other_save, b"another game's progress").unwrap();
    std::fs::write(steam.join("steamapps/appmanifest_553850.acf"),
        "\"AppState\"\n{\n\"appid\" \"553850\"\n\"name\" \"Helldivers 2\"\n\"installdir\" \"Helldivers 2\"\n}\n").unwrap();
    let alias = agent.config.home.join(".steam/steam");
    std::os::unix::fs::symlink("debian-installation", &alias).unwrap();
    let prefix = steam.join("steamapps/compatdata/553850/pfx");
    std::fs::create_dir_all(prefix.join("dosdevices")).unwrap();
    std::fs::create_dir_all(prefix.join("drive_c/users/steamuser/Local Settings")).unwrap();
    std::fs::create_dir_all(prefix.join("drive_c/users/steamuser/AppData/Local")).unwrap();
    std::os::unix::fs::symlink("../drive_c", prefix.join("dosdevices/c:")).unwrap();
    std::os::unix::fs::symlink("/dev/ttyS0", prefix.join("dosdevices/com1")).unwrap();
    std::os::unix::fs::symlink("/dev/vdb", prefix.join("dosdevices/d::")).unwrap();
    std::os::unix::fs::symlink(
        "../AppData/Local",
        prefix.join("drive_c/users/steamuser/Local Settings/Application Data"),
    )
    .unwrap();
    let key = MasterKey::generate();
    let cloud = root.join("cloud");
    agent.discover().unwrap();
    let manifest = run_backup_to_local(
        &agent,
        &AppId::steam(553850),
        BackupMode::CompleteApplication,
        cloud.clone(),
        &key,
    )
    .await
    .unwrap();
    for (relative, bytes) in files {
        let path = steam.join(relative);
        let entry = manifest
            .files
            .iter()
            .find(|entry| entry.source_path_hint.as_deref() == path.to_str())
            .unwrap();
        assert_eq!(
            entry.file_hash,
            format!("blake3:{}", blake3::hash(bytes).to_hex())
        );
        if relative.starts_with("userdata/") {
            assert_eq!(entry.logical_root, LogicalRoot::SteamUserdata.as_token());
            assert_eq!(entry.relative_path, "123/553850/remote/save.dat");
        }
    }
    assert!(manifest.files.iter().all(|entry| {
        let hint = entry.source_path_hint.as_deref().unwrap();
        !Path::new(hint).starts_with(&alias)
            && Path::new(hint) != other_save
            && !matches!(
                entry.symlink_target.as_deref(),
                Some("/dev/ttyS0" | "/dev/vdb")
            )
    }));
    assert_eq!(manifest.environment["excluded_host_device_links"], 2);
    assert_eq!(
        std::fs::read_link(alias).unwrap(),
        Path::new("debian-installation")
    );
    assert_eq!(
        std::fs::read(other_save).unwrap(),
        b"another game's progress"
    );
    let restored = root.join("restored-prefix");
    let mut restore_roots = agent.roots.lock().clone();
    restore_roots
        .proton_prefixes
        .insert(553850, restored.clone());
    let provider = LocalStorage::new(cloud);
    let plan = noland_restore::prepare_restore(
        &provider,
        &key,
        &agent.config.paths,
        &AppId::steam(553850),
        manifest.bundle_id,
        RestoreMode::CompleteApplication,
    )
    .await
    .unwrap()
    .into_shared();
    let index = read_pack_index(&provider, &key, &AppId::steam(553850), manifest.bundle_id)
        .await
        .unwrap();
    noland_restore::restore_pipelined(
        &provider,
        &key,
        &plan,
        &index,
        &restore_roots,
        None,
        None,
        noland_restore::PipelineOptions::default(),
        |_| Ok(()),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_link(restored.join("dosdevices/c:")).unwrap(),
        Path::new("../drive_c")
    );
    assert_eq!(
        std::fs::read_link(
            restored.join("drive_c/users/steamuser/Local Settings/Application Data")
        )
        .unwrap(),
        Path::new("../AppData/Local")
    );
    assert_eq!(
        std::fs::read(restored.join("drive_c/users/steamuser/AppData/Local/save.dat")).unwrap(),
        b"prefix save"
    );
    assert!(std::fs::symlink_metadata(restored.join("dosdevices/com1")).is_err());
    assert!(std::fs::symlink_metadata(restored.join("dosdevices/d::")).is_err());
    assert_eq!(
        std::fs::read_link(prefix.join("dosdevices/com1")).unwrap(),
        Path::new("/dev/ttyS0")
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn small_hash_job_needs_no_snapshot_artifact_and_survives_source_removal() {
    let (root, agent, app, source, _, _) = fixture("direct-hash");
    reconcile_app_for_complete_backup(&agent, &app).unwrap();
    let record = agent
        .db
        .get_path_by_canonical(source.to_str().unwrap())
        .unwrap()
        .unwrap();
    let association = agent
        .db
        .associations_for_path(record.path_id)
        .unwrap()
        .remove(0);
    let snapshot_root = root.join("not-a-directory");
    std::fs::write(&snapshot_root, b"snapshot creation must not be attempted").unwrap();
    let cas = LocalCas::new(root.join("capture-cas")).unwrap();
    let lease = cas.lease().unwrap();
    let result = hash_one_file(
        HashJob {
            app_id: app,
            logical: agent.roots.lock().classify(&source).unwrap(),
            source: source.clone(),
            snapshot_root,
            staged: None,
            record,
            association,
            shared_app_ids: Vec::new(),
        },
        &lease,
        &mut ChunkFileReader::default(),
        &Default::default(),
        &RemoteChunkIndex::empty(),
        Utc::now(),
        &(Arc::new(|| false) as CancellationCheck),
    )
    .unwrap();
    assert_eq!(result.consistency, ConsistencyKind::Snapshot);
    assert_eq!(result.file_state.trust, FileStateTrust::Trusted);
    lease.sync_all().unwrap();
    std::fs::remove_file(source).unwrap();
    assert_eq!(result.new_chunks.len(), 1);
    let (hash, captured) = &result.new_chunks[0];
    captured.verify(hash).unwrap();
    assert_eq!(captured.read().unwrap(), b"immutable content");
    drop(lease);
    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn complete_capture_discovers_unindexed_app_caches_and_preserves_exact_content() {
    let (root, agent, app, source, storage, key) = fixture("complete-cache-scope");
    let (first, _) = backup(&agent, &app, &storage, &key).await;
    let install = source.parent().unwrap().parent().unwrap();
    let paths = [
        "cache/pipeline.bin",
        ".cache/shaders/compiled",
        "mods/local/config.json",
    ];
    for relative in paths {
        let path = install.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, relative.as_bytes()).unwrap();
    }
    // No observer event or pre-existing association: complete reconciliation
    // must discover trees previously excluded by their cache names.
    let (complete, _) = backup(&agent, &app, &storage, &key).await;
    assert_ne!(complete.bundle_id, first.bundle_id);
    assert_eq!(complete.environment["capture_scope_version"], 1);
    let operation = agent
        .db
        .recent_operations(10)
        .unwrap()
        .into_iter()
        .find(|operation| {
            operation.detail_json["bundle_id"] == serde_json::json!(complete.bundle_id)
        })
        .unwrap();
    assert!(agent
        .db
        .get_operation_progress(operation.operation_id)
        .unwrap()
        .is_none());
    assert_eq!(
        operation.detail_json["streaming_pipeline"]["waiting_on"],
        "complete"
    );
    assert_eq!(
        operation.detail_json["streaming_pipeline"]["active_readers"],
        0
    );
    assert_eq!(
        operation.detail_json["streaming_pipeline"]["selected_files"],
        complete.files.len()
    );
    let index = read_pack_index(&storage, &key, &app, complete.bundle_id)
        .await
        .unwrap();
    for relative in paths {
        let path = install.join(relative);
        let file = complete
            .files
            .iter()
            .find(|file| file.source_path_hint.as_deref() == path.to_str())
            .unwrap();
        assert_eq!(file.file_hash, noland_cas::blake3_hex(relative.as_bytes()));
        let entry = index
            .iter()
            .find(|entry| entry.chunk_hash == file.chunks[0].hash)
            .unwrap();
        let pack = root.join("downloaded.pack");
        storage
            .download(
                &noland_storage::RemoteKey::new(pack_key(&entry.pack_id)),
                &pack,
            )
            .await
            .unwrap();
        assert_eq!(
            noland_pack::extract_chunk(&pack, entry, &key).unwrap(),
            relative.as_bytes()
        );
        assert_eq!(std::fs::read(path).unwrap(), relative.as_bytes());
    }
    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn nested_state_root_transition_reuses_bytes_but_commits_current_classification() {
    let (root, agent, app, path, storage, key) = fixture("nested-root");
    let (first, _) = backup(&agent, &app, &storage, &key).await;
    assert_eq!(first.files[0].semantic_role, SemanticRole::AppContent);
    assert!(agent.db.list_dirty_apps().unwrap().is_empty());
    agent
        .db
        .add_known_root(&app, "state", path.parent().unwrap().to_str().unwrap())
        .unwrap();
    let (second, metrics) = backup(&agent, &app, &storage, &key).await;
    assert_ne!(
        first.bundle_id, second.bundle_id,
        "metadata-only revisions must not become no-ops"
    );
    assert_eq!(metrics.num_files_rehashed, 0);
    assert_eq!(metrics.num_files_skipped_fast_identity, 1);
    assert_eq!(first.files[0].file_hash, second.files[0].file_hash);
    assert_eq!(first.files[0].chunks, second.files[0].chunks);
    assert_eq!(
        second.files[0].persistence_class,
        PersistenceClass::PersistentState
    );
    assert_eq!(second.files[0].semantic_role, SemanticRole::UserState);
    let committed = read_committed_manifest(&storage, &key, &app, second.bundle_id)
        .await
        .unwrap();
    assert_eq!(committed.files[0].semantic_role, SemanticRole::UserState);
    let (third, metrics) = backup(&agent, &app, &storage, &key).await;
    assert_eq!(third.bundle_id, second.bundle_id);
    assert_eq!(metrics.bytes_hashed, 0);
    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn fast_reuse_refreshes_mode_confidence_and_shared_owners_without_content_reads() {
    use std::os::unix::fs::PermissionsExt;
    let (root, agent, app, path, storage, key) = fixture("metadata-only");
    agent
        .db
        .add_known_root(&app, "state", path.parent().unwrap().to_str().unwrap())
        .unwrap();
    let (first, _) = backup(&agent, &app, &storage, &key).await;
    let id = agent
        .db
        .get_path_by_canonical(path.to_str().unwrap())
        .unwrap()
        .unwrap()
        .path_id;
    let mut association = agent
        .db
        .associations_for_path(id)
        .unwrap()
        .into_iter()
        .find(|a| a.app_id == app)
        .unwrap();
    association.confidence = 0.99;
    agent.db.upsert_association(&association).unwrap();
    let other = AppId::desktop("shared-fixture");
    agent
        .db
        .upsert_app(&AppIdentity::new(other.clone(), "Shared Fixture"))
        .unwrap();
    association.app_id = other.clone();
    agent.db.upsert_association(&association).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
    // Personal capture checks current metadata without complete reconciliation
    // downgrading a chmod-only identity. Seed a parent in this same mode first.
    let personal = run_backup(
        &agent,
        &app,
        BackupMode::PersonalState,
        BackupPerformanceMode::Balanced,
        &storage,
        &key,
        None,
    )
    .await
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let operation = Uuid::new_v4();
    let changed = run_backup(
        &agent,
        &app,
        BackupMode::PersonalState,
        BackupPerformanceMode::Balanced,
        &storage,
        &key,
        Some(operation),
    )
    .await
    .unwrap();
    let metrics = operation_metrics(&agent.db.get_operation(operation).unwrap().unwrap());
    assert_ne!(personal.bundle_id, changed.bundle_id);
    assert_eq!(metrics.bytes_hashed, 0);
    assert_eq!(changed.files[0].mode.unwrap() & 0o777, 0o600);
    assert_eq!(changed.files[0].association_confidence, 0.99);
    assert_eq!(changed.files[0].shared_app_ids, vec![other]);
    assert_eq!(first.files[0].file_hash, changed.files[0].file_hash);
    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn authenticated_parent_cache_skips_rewrites_and_repairs_missing_or_forged_index_rows() {
    let (root, agent, app, _, storage, key) = fixture("cache-receipts");
    let (first, _) = backup(&agent, &app, &storage, &key).await;
    let binding = repository_cache_binding(storage.storage_identity(), &key).unwrap();
    let chunk = &first.files[0].chunks[0].hash;
    let original = agent
        .db
        .get_remote_content_by_hash(&binding, ContentObjectKind::Chunk, chunk)
        .unwrap()
        .unwrap();
    // An old timestamp makes an accidental refresh observable without a sleep.
    let mut old = original.clone();
    old.observed_at = chrono::DateTime::from_timestamp(1, 0).unwrap();
    agent.db.upsert_remote_content_entry(&old).unwrap();
    assert!(load_parent(&storage, &key, &agent, &app).await.is_some());
    let reused = agent
        .db
        .get_remote_content_by_hash(&binding, ContentObjectKind::Chunk, chunk)
        .unwrap()
        .unwrap();
    assert_eq!(reused.observed_at, old.observed_at);
    agent
        .db
        .remove_remote_content_entry(&binding, ContentObjectKind::Chunk, chunk)
        .unwrap();
    assert!(load_parent(&storage, &key, &agent, &app).await.is_some());
    assert!(agent
        .db
        .get_remote_content_by_hash(&binding, ContentObjectKind::Chunk, chunk)
        .unwrap()
        .is_some());
    let mut forged = original;
    forged.etag = Some("{\"pack_id\":\"forged\"}".into());
    agent.db.upsert_remote_content_entry(&forged).unwrap();
    assert!(
        load_remote_chunk_index(&agent, Some(&binding), &key, Utc::now())
            .unwrap()
            .entries
            .is_empty()
    );
    assert!(load_parent(&storage, &key, &agent, &app).await.is_some());
    assert_eq!(
        load_remote_chunk_index(&agent, Some(&binding), &key, Utc::now())
            .unwrap()
            .entries
            .len(),
        1
    );
    let dir = parent_cache_dir(&agent, &app, first.bundle_id).join(&binding);
    std::fs::write(dir.join("index.json"), b"[]").unwrap();
    assert!(read_cached_parent(&agent, &app, first.bundle_id, Some(&binding), &key).is_none());
    // Invalid cache falls back to the authenticated remote index and reseeds it.
    agent
        .db
        .remove_remote_content_entry(&binding, ContentObjectKind::Chunk, chunk)
        .unwrap();
    let (_, mut index) = load_parent(&storage, &key, &agent, &app).await.unwrap();
    assert_eq!(index.len(), 1);
    // A newly authenticated immutable index may relocate an existing hash.
    // Same-hash deduplication must not suppress the new pack mapping.
    index[0].pack_id = Uuid::new_v4().to_string();
    remember_remote_pack_index(&agent, storage.storage_identity(), &key, &index).unwrap();
    assert_eq!(
        load_remote_chunk_index(&agent, Some(&binding), &key, Utc::now())
            .unwrap()
            .get(chunk)
            .unwrap()
            .pack_id,
        index[0].pack_id
    );
    assert_eq!(
        load_remote_chunk_index(&agent, Some(&binding), &key, Utc::now())
            .unwrap()
            .entries
            .len(),
        1
    );
    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn all_app_retry_reuses_completed_child_and_preserves_new_mutations() {
    let (root, agent, app, source, storage, key) = fixture("group-retry");
    let parent = Uuid::new_v4();
    agent
        .db
        .upsert_operation(&OperationRecord {
            operation_id: parent,
            kind: "backup".into(),
            app_id: None,
            state: "QUEUED".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            last_error: None,
            detail_json: serde_json::json!({"app_id":"*", "mode":"complete_application"}),
        })
        .unwrap();
    let first = run_backup_all(
        &agent,
        BackupMode::CompleteApplication,
        BackupPerformanceMode::Balanced,
        &storage,
        &key,
        Some(parent),
    )
    .await
    .unwrap();
    assert_eq!(first.len(), 1);
    let saved =
        agent.db.get_operation(parent).unwrap().unwrap().detail_json["backup_children_v1"].clone();
    drop(agent);
    let agent = StateAgent::boot(crate::AgentConfig::isolated(root.clone())).unwrap();
    agent.recover().unwrap();
    std::fs::write(&source, b"new revision after parent failure").unwrap();
    agent.db.mark_dirty(&app, None, true).unwrap();
    // New apps and changed dirty selection must not replace the original plan.
    let another = AppIdentity::new(AppId::desktop("later-app"), "Later App");
    agent.db.upsert_app(&another).unwrap();
    agent.db.mark_dirty(&another.app_id, None, true).unwrap();
    let retry = run_backup_all(
        &agent,
        BackupMode::CompleteApplication,
        BackupPerformanceMode::Balanced,
        &storage,
        &key,
        Some(parent),
    )
    .await
    .unwrap();
    assert_eq!(retry.len(), 1);
    assert_eq!(retry[0].bundle_id, first[0].bundle_id);
    assert_eq!(retry[0].files[0].file_hash, first[0].files[0].file_hash);
    assert_eq!(
        agent.db.get_operation(parent).unwrap().unwrap().detail_json["backup_children_v1"],
        saved
    );
    assert!(agent
        .db
        .list_dirty_apps()
        .unwrap()
        .iter()
        .any(|row| row.app_id == app));
    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn scoped_chunk_lookup_is_authenticated_and_has_a_fixed_cache_bound() {
    let (root, agent, _, _, storage, key) = fixture("scoped-index");
    let binding = repository_cache_binding(storage.storage_identity(), &key).unwrap();
    let index =
        scoped_index::ScopedIndex::new(agent.db.clone(), binding.clone(), key.clone(), Utc::now());
    // Negative lookups also have a hard bound; a long cold capture cannot retain
    // one missing-entry allocation per repository chunk.
    for i in 0..5000 {
        assert!(index.get(&format!("blake3:{i:064x}")).unwrap().is_none());
    }
    assert_eq!(index.cached_entries(), 4096);
    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}

struct MeasuredStorage {
    inner: LocalStorage,
    fail_metadata: std::sync::atomic::AtomicBool,
    counts: parking_lot::Mutex<noland_storage::StorageOperationMetrics>,
    payload_metadata_reads: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl SharedStorageProvider for MeasuredStorage {
    async fn health_check(&self) -> Result<noland_storage::Health> {
        self.inner.health_check().await
    }
    async fn ensure_root(&self) -> Result<()> {
        self.inner.ensure_root().await
    }
    async fn stat(
        &self,
        key: &noland_storage::RemoteKey,
    ) -> Result<Option<noland_storage::RemoteMeta>> {
        self.counts.lock().remote_stat_calls += 1;
        self.inner.stat(key).await
    }
    async fn upload_immutable(
        &self,
        path: &Path,
        key: &noland_storage::RemoteKey,
    ) -> Result<noland_storage::RemoteMeta> {
        self.inner.upload_immutable(path, key).await
    }
    async fn download(&self, key: &noland_storage::RemoteKey, path: &Path) -> Result<()> {
        if key.as_str().ends_with("/manifest.enc") || key.as_str().ends_with("/index.enc") {
            self.payload_metadata_reads
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        self.inner.download(key, path).await?;
        let mut counts = self.counts.lock();
        counts.remote_download_calls += 1;
        counts.bytes_downloaded += std::fs::metadata(path)?.len();
        Ok(())
    }
    async fn list_prefix(
        &self,
        key: &noland_storage::RemoteKey,
    ) -> Result<Vec<noland_storage::RemoteEntry>> {
        self.inner.list_prefix(key).await
    }
    async fn put_small_versioned(
        &self,
        bytes: bytes::Bytes,
        key: &noland_storage::RemoteKey,
    ) -> Result<noland_storage::RemoteMeta> {
        if key.as_str().starts_with("bundles/")
            && self
                .fail_metadata
                .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(StateError::Storage("injected metadata failure".into()));
        }
        self.inner.put_small_versioned(bytes, key).await
    }
    async fn delete_object(&self, key: &noland_storage::RemoteKey) -> Result<()> {
        self.inner.delete_object(key).await
    }
    fn operation_metrics(&self) -> noland_storage::StorageOperationMetrics {
        *self.counts.lock()
    }
    fn storage_identity(&self) -> Option<ProviderRootIdentity> {
        self.inner.storage_identity()
    }
}

#[tokio::test]
async fn failed_all_app_child_resumes_original_capture_after_source_changes() {
    let (root, agent, app, source, storage, key) = fixture("failed-group-capture");
    let storage = MeasuredStorage {
        inner: storage,
        fail_metadata: true.into(),
        counts: Default::default(),
        payload_metadata_reads: 0.into(),
    };
    let original_hash = noland_cas::blake3_file(&source).unwrap();
    let parent = Uuid::new_v4();
    agent
        .db
        .upsert_operation(&OperationRecord {
            operation_id: parent,
            kind: "backup".into(),
            app_id: None,
            state: "QUEUED".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            last_error: None,
            detail_json: serde_json::json!({"app_id":"*", "mode":"complete_application"}),
        })
        .unwrap();
    assert!(run_backup_all(
        &agent,
        BackupMode::CompleteApplication,
        BackupPerformanceMode::Balanced,
        &storage,
        &key,
        Some(parent)
    )
    .await
    .is_err());
    let selection = group::plan(&agent, Some(parent), RegenerableCachePolicy::Include).unwrap();
    let child = selection[0].operation_id;
    let captured = read_prepared_backup(
        &agent,
        &storage,
        &key,
        child,
        &app,
        BackupMode::CompleteApplication,
    )
    .unwrap()
    .unwrap();
    let bundle = captured.manifest.bundle_id;
    std::fs::write(&source, b"changed after failed upload").unwrap();
    let retry = run_backup_all(
        &agent,
        BackupMode::CompleteApplication,
        BackupPerformanceMode::Balanced,
        &storage,
        &key,
        Some(parent),
    )
    .await
    .unwrap();
    assert_eq!(retry[0].bundle_id, bundle);
    assert_eq!(retry[0].files[0].file_hash, original_hash);
    assert!(agent
        .db
        .list_dirty_apps()
        .unwrap()
        .iter()
        .any(|dirty| dirty.app_id == app));
    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn unchanged_backup_avoids_manifest_redownload_and_counts_parent_cache_misses() {
    let (root, agent, app, _, storage, key) = fixture("parent-accounting");
    let storage = MeasuredStorage {
        inner: storage,
        fail_metadata: false.into(),
        counts: Default::default(),
        payload_metadata_reads: 0.into(),
    };
    let run = || {
        run_backup(
            &agent,
            &app,
            BackupMode::CompleteApplication,
            BackupPerformanceMode::Balanced,
            &storage,
            &key,
            None,
        )
    };
    let first = run().await.unwrap();
    *storage.counts.lock() = Default::default();
    storage
        .payload_metadata_reads
        .store(0, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(run().await.unwrap().bundle_id, first.bundle_id);
    assert_eq!(
        storage
            .payload_metadata_reads
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    let binding = repository_cache_binding(storage.storage_identity(), &key).unwrap();
    let cache = parent_cache_dir(&agent, &app, first.bundle_id).join(binding);
    std::fs::remove_file(cache.join("manifest.json")).unwrap();
    let id = Uuid::new_v4();
    let before = storage.operation_metrics();
    let unchanged = run_backup(
        &agent,
        &app,
        BackupMode::CompleteApplication,
        BackupPerformanceMode::Balanced,
        &storage,
        &key,
        Some(id),
    )
    .await
    .unwrap();
    assert_eq!(unchanged.bundle_id, first.bundle_id);
    let metrics = operation_metrics(&agent.db.get_operation(id).unwrap().unwrap());
    assert_eq!(
        storage
            .payload_metadata_reads
            .load(std::sync::atomic::Ordering::SeqCst),
        2
    ); // Parent manifest + index, no second manifest.
    assert!(metrics.num_remote_download_calls >= 2); // Includes repository coordination/catalog metadata.
    assert_eq!(
        metrics.bytes_downloaded,
        storage
            .counts
            .lock()
            .bytes_downloaded
            .saturating_sub(before.bytes_downloaded)
    );
    assert!(metrics.bytes_downloaded > 0);
    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}
