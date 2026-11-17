//! Each fresh inventory overlaps only one bundle's authenticated documents.
use super::{
    tests::{bundle, fixture, pack, target},
    *,
};
use crate::{LocalStorage, RemoteEntry, RemoteMeta, RepositoryGuard};
use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex,
    },
    time::Duration,
};

struct Rewrite {
    key: RemoteKey,
    bytes: Bytes,
}

struct InventoryProbe {
    inner: LocalStorage,
    parallel: bool,
    pair_reads: AtomicBool,
    pair: tokio::sync::Barrier,
    corrupt_index: bool,
    finish_manifest: tokio::sync::Notify,
    reads: Mutex<BTreeMap<String, usize>>,
    events: Mutex<Vec<String>>,
    active: AtomicUsize,
    peak: AtomicUsize,
    finished: AtomicUsize,
    inventories: AtomicUsize,
    rewrite_inventory: Mutex<Option<(usize, Rewrite)>>,
    rewrite_index: Mutex<Option<Rewrite>>,
    writes: AtomicUsize,
    deletes: AtomicUsize,
}

impl InventoryProbe {
    fn new(inner: LocalStorage) -> Self {
        Self {
            inner,
            parallel: true,
            pair_reads: AtomicBool::new(false),
            pair: tokio::sync::Barrier::new(2),
            corrupt_index: false,
            finish_manifest: tokio::sync::Notify::new(),
            reads: Mutex::new(BTreeMap::new()),
            events: Mutex::new(Vec::new()),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            finished: AtomicUsize::new(0),
            inventories: AtomicUsize::new(0),
            rewrite_inventory: Mutex::new(None),
            rewrite_index: Mutex::new(None),
            writes: AtomicUsize::new(0),
            deletes: AtomicUsize::new(0),
        }
    }

    fn count(&self, key: &str) -> usize {
        self.reads.lock().unwrap().get(key).copied().unwrap_or(0)
    }

    async fn rewrite(&self, rewrite: Rewrite) -> Result<()> {
        self.inner
            .put_small_versioned(rewrite.bytes, &rewrite.key)
            .await?;
        Ok(())
    }

    fn assert_read_only(&self) {
        assert_eq!(self.writes.load(Ordering::SeqCst), 0);
        assert_eq!(self.deletes.load(Ordering::SeqCst), 0);
    }
}

struct ActiveRead<'a>(&'a AtomicUsize);
impl Drop for ActiveRead<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl SharedStorageProvider for InventoryProbe {
    async fn health_check(&self) -> Result<crate::Health> {
        self.inner.health_check().await
    }

    async fn ensure_root(&self) -> Result<()> {
        self.inner.ensure_root().await
    }

    async fn stat(&self, key: &RemoteKey) -> Result<Option<RemoteMeta>> {
        self.inner.stat(key).await
    }

    async fn download(&self, key: &RemoteKey, dest: &Path) -> Result<()> {
        self.inner.download(key, dest).await
    }

    async fn upload_immutable(&self, local: &Path, key: &RemoteKey) -> Result<RemoteMeta> {
        self.inner.upload_immutable(local, key).await
    }

    async fn list_prefix(&self, key: &RemoteKey) -> Result<Vec<RemoteEntry>> {
        let entries = self.inner.list_prefix(key).await?;
        if key.as_str() == "bundles" {
            let pass = self.inventories.fetch_add(1, Ordering::SeqCst) + 1;
            let rewrite = {
                let mut rewrite = self.rewrite_inventory.lock().unwrap();
                if rewrite.as_ref().is_some_and(|(at, _)| *at == pass) {
                    rewrite.take().map(|(_, rewrite)| rewrite)
                } else {
                    None
                }
            };
            if let Some(rewrite) = rewrite {
                self.rewrite(rewrite).await?;
            }
        }
        Ok(entries)
    }

    async fn read_small(&self, key: &RemoteKey, limit: u64) -> Result<Vec<u8>> {
        let name = key.as_str();
        *self.reads.lock().unwrap().entry(name.into()).or_default() += 1;
        self.events.lock().unwrap().push(format!("start:{name}"));
        let index = name.ends_with("/index.enc");
        let document = name.starts_with("bundles/") && (index || name.ends_with("/manifest.enc"));
        if !document {
            return self.inner.read_small(key, limit).await;
        }
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(active, Ordering::SeqCst);
        let _active = ActiveRead(&self.active);
        if self.pair_reads.load(Ordering::SeqCst) {
            self.pair.wait().await;
        }
        if self.corrupt_index && !index {
            // The sibling error must not cancel this admitted read. A try_join!
            // would return before this future can record a completed response.
            self.finish_manifest.notified().await;
        }
        let result = if self.corrupt_index && index {
            self.finish_manifest.notify_one();
            Ok(b"not authenticated metadata".to_vec())
        } else {
            self.inner.read_small(key, limit).await
        };
        if index {
            let rewrite = self.rewrite_index.lock().unwrap().take();
            if let Some(rewrite) = rewrite {
                self.rewrite(rewrite).await?;
            }
        }
        self.finished.fetch_add(1, Ordering::SeqCst);
        self.events.lock().unwrap().push(format!("finish:{name}"));
        result
    }

    async fn put_small_versioned(&self, bytes: Bytes, key: &RemoteKey) -> Result<RemoteMeta> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        self.inner.put_small_versioned(bytes, key).await
    }

    async fn delete_object(&self, key: &RemoteKey) -> Result<()> {
        self.deletes.fetch_add(1, Ordering::SeqCst);
        self.inner.delete_object(key).await
    }

    fn transfer_tuning(&self) -> noland_rclone_adapter::TransferTuning {
        noland_rclone_adapter::TransferTuning {
            max_parallel_downloads: if self.parallel { 2 } else { 1 },
            ..Default::default()
        }
    }
}

async fn data_bundle(
    root: &Path,
    storage: &LocalStorage,
    master: &MasterKey,
    app: &str,
    parent: Option<Uuid>,
) -> BundleManifest {
    let bytes = format!("payload for {app}");
    let payload = pack(root, master, bytes.as_bytes());
    let mut manifest = bundle(storage, master, app, &[&payload], parent).await;
    manifest.files.push(ManifestFile {
        logical_root: "$HOME".into(),
        relative_path: "save.dat".into(),
        source_path_hint: None,
        file_type: "file".into(),
        size: bytes.len() as u64,
        file_hash: format!("blake3:{}", blake3::hash(bytes.as_bytes()).to_hex()),
        chunks: vec![ChunkRef {
            hash: payload.entries[0].chunk_hash.clone(),
            size: bytes.len() as u64,
        }],
        mode: None,
        mtime_ns: None,
        uid: None,
        gid: None,
        symlink_target: None,
        persistence_class: PersistenceClass::PersistentState,
        semantic_role: SemanticRole::UserState,
        association_confidence: 1.0,
        shared_app_ids: Vec::new(),
    });
    let rewrite = manifest_rewrite(master, &manifest);
    storage
        .put_small_versioned(rewrite.bytes, &rewrite.key)
        .await
        .unwrap();
    crate::update_catalog_with_bundle(storage, master, &manifest, payload.bytes)
        .await
        .unwrap();
    manifest
}

fn manifest_rewrite(master: &MasterKey, manifest: &BundleManifest) -> Rewrite {
    Rewrite {
        key: RemoteKey::new(format!(
            "{}/manifest.enc",
            bundle_dir(&manifest.app.app_id, manifest.bundle_id)
        )),
        bytes: Bytes::from(
            seal_metadata(&derive_keys(master).manifest, b"manifest", manifest)
                .unwrap()
                .envelope,
        ),
    }
}

async fn bounded_inventory(
    storage: &InventoryProbe,
    master: &MasterKey,
) -> Result<Vec<InventoryBundle>> {
    tokio::time::timeout(Duration::from_secs(5), inventory(storage, master))
        .await
        .expect("inventory metadata reads must make progress")
}

#[tokio::test]
async fn inventory_pairs_only_two_documents_and_matches_serial_inventory() {
    let (root, local, master) = fixture();
    let mut manifests = Vec::new();
    for id in 0..6 {
        manifests.push(data_bundle(&root, &local, &master, &format!("app-{id}"), None).await);
    }
    let storage = InventoryProbe::new(local);
    storage.pair_reads.store(true, Ordering::SeqCst);
    let parallel = bounded_inventory(&storage, &master).await.unwrap();
    assert_eq!(storage.peak.load(Ordering::SeqCst), 2);
    assert_eq!(storage.active.load(Ordering::SeqCst), 0);
    assert_eq!(storage.finished.load(Ordering::SeqCst), manifests.len() * 2);
    assert!(parallel
        .windows(2)
        .all(|pair| pair[0].target.deletion.bundle_id < pair[1].target.deletion.bundle_id));
    for manifest in &manifests {
        let prefix = bundle_dir(&manifest.app.app_id, manifest.bundle_id);
        assert_eq!(storage.count(&format!("{prefix}/manifest.enc")), 1);
        assert_eq!(storage.count(&format!("{prefix}/index.enc")), 1);
        assert_eq!(storage.count(&format!("{prefix}/COMMITTED")), 1);
        assert_eq!(
            storage.count(&format!("deletions/bundles/{}.enc", manifest.bundle_id)),
            1
        );
        let events = storage.events.lock().unwrap();
        let position = |event: String| events.iter().position(|e| e == &event).unwrap();
        let marker = position(format!("start:{prefix}/COMMITTED"));
        assert!(position(format!("finish:{prefix}/manifest.enc")) < marker);
        assert!(position(format!("finish:{prefix}/index.enc")) < marker);
    }
    storage.assert_read_only();
    let mut serial = InventoryProbe::new(LocalStorage::new(storage.inner.root.clone()));
    serial.parallel = false;
    let serial_inventory = bounded_inventory(&serial, &master).await.unwrap();
    assert_eq!(serial.peak.load(Ordering::SeqCst), 1);
    assert_eq!(
        crate::compact_json_hash(&parallel).unwrap(),
        crate::compact_json_hash(&serial_inventory).unwrap()
    );
    serial.assert_read_only();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn a_failed_metadata_read_drains_its_sibling_without_publishing_a_plan() {
    let (root, local, master) = fixture();
    let manifest = data_bundle(&root, &local, &master, "failed-index", None).await;
    let mut storage = InventoryProbe::new(local);
    storage.pair_reads.store(true, Ordering::SeqCst);
    storage.corrupt_index = true;
    assert!(bounded_inventory(&storage, &master).await.is_err());
    assert_eq!(storage.peak.load(Ordering::SeqCst), 2);
    assert_eq!(storage.active.load(Ordering::SeqCst), 0);
    assert_eq!(storage.finished.load(Ordering::SeqCst), 2);
    assert_eq!(
        storage.count(&format!(
            "{}/COMMITTED",
            bundle_dir(&manifest.app.app_id, manifest.bundle_id)
        )),
        0,
        "do not accept a marker after a failed authentication"
    );
    storage.assert_read_only();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn convergence_keeps_two_fresh_inventories_and_rejects_authenticated_replacement_changes() {
    let (root, local, master) = fixture();
    let old = data_bundle(&root, &local, &master, "replacement", None).await;
    let replacement = data_bundle(&root, &local, &master, "replacement", Some(old.bundle_id)).await;
    let operation = Uuid::new_v4();
    let guard = RepositoryGuard::acquire(&local, &master, operation, "fixture")
        .await
        .unwrap();
    let storage = InventoryProbe::new(LocalStorage::new(local.root.clone()));
    let mut changed = replacement.clone();
    changed.environment["changed_after_plan"] = serde_json::json!(true);
    *storage.rewrite_inventory.lock().unwrap() = Some((2, manifest_rewrite(&master, &changed)));
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        converge_application(&storage, &master, &replacement, operation, "fixture"),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(error
        .to_string()
        .contains("cloud backups changed since confirmation"));
    assert_eq!(storage.inventories.load(Ordering::SeqCst), 2);
    for manifest in [&old, &replacement] {
        let prefix = bundle_dir(&manifest.app.app_id, manifest.bundle_id);
        assert_eq!(storage.count(&format!("{prefix}/index.enc")), 2);
        assert_eq!(
            storage.count(&format!("{prefix}/manifest.enc")),
            if manifest.bundle_id == replacement.bundle_id {
                3 // Replacement validation, then independently in both passes.
            } else {
                2
            }
        );
    }
    storage.assert_read_only();
    assert!(storage
        .inner
        .stat(&plan_key(replacement.bundle_id))
        .await
        .unwrap()
        .is_none());
    assert!(storage
        .inner
        .stat(&pending_key(replacement.bundle_id))
        .await
        .unwrap()
        .is_none());
    guard.release().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn a_marker_generation_change_during_parallel_reads_fails_closed() {
    let (root, local, master) = fixture();
    let manifest = data_bundle(&root, &local, &master, "marker-race", None).await;
    let storage = InventoryProbe::new(local);
    storage.pair_reads.store(true, Ordering::SeqCst);
    let marker = format!(
        "{}/COMMITTED",
        bundle_dir(&manifest.app.app_id, manifest.bundle_id)
    );
    *storage.rewrite_index.lock().unwrap() = Some(Rewrite {
        key: RemoteKey::new(&marker),
        bytes: Bytes::from(Uuid::new_v4().to_string()),
    });
    let error = bounded_inventory(&storage, &master).await.err().unwrap();
    assert!(error
        .to_string()
        .contains("commit marker differs from authenticated manifest"));
    assert_eq!(storage.finished.load(Ordering::SeqCst), 2);
    assert_eq!(storage.count(&marker), 1);
    storage.assert_read_only();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn inventory_rejects_missing_forged_and_incompatible_indices() {
    let (root, local, master) = fixture();
    let manifest = data_bundle(&root, &local, &master, "bad-indices", None).await;
    let index_key = RemoteKey::new(format!(
        "{}/index.enc",
        bundle_dir(&manifest.app.app_id, manifest.bundle_id)
    ));
    let mut index =
        crate::read_pack_index(&local, &master, &manifest.app.app_id, manifest.bundle_id)
            .await
            .unwrap();
    let forged = seal_metadata(
        &derive_keys(&MasterKey::generate()).manifest,
        b"pack-index",
        &index,
    )
    .unwrap()
    .envelope;
    index[0].plaintext_len += 1;
    let incompatible = seal_metadata(&derive_keys(&master).manifest, b"pack-index", &index)
        .unwrap()
        .envelope;
    for bytes in [b"forged plaintext".to_vec(), forged, incompatible] {
        local
            .put_small_versioned(Bytes::from(bytes), &index_key)
            .await
            .unwrap();
        let storage = InventoryProbe::new(LocalStorage::new(local.root.clone()));
        assert!(bounded_inventory(&storage, &master).await.is_err());
        assert_eq!(storage.count(index_key.as_str()), 1);
        assert_eq!(storage.active.load(Ordering::SeqCst), 0);
        storage.assert_read_only();
    }
    std::fs::remove_file(local.root.join(index_key.as_str())).unwrap();
    let storage = InventoryProbe::new(local);
    let error = bounded_inventory(&storage, &master).await.err().unwrap();
    assert!(error
        .to_string()
        .contains("committed data bundle has no pack index"));
    assert_eq!(storage.count(index_key.as_str()), 0);
    storage.assert_read_only();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn indexed_metadata_prefix_retains_unknown_objects_without_widening_scope() {
    let (root, local, master) = fixture();
    let manifest = data_bundle(&root, &local, &master, "unknown-metadata", None).await;
    let prefix = bundle_dir(&manifest.app.app_id, manifest.bundle_id);
    let unknown = format!("{prefix}/unknown.metadata");
    let outside = format!("{prefix}-other/unknown.metadata");
    for name in [&unknown, &outside] {
        local
            .put_small_versioned(
                Bytes::from_static(b"retained in preview"),
                &RemoteKey::new(name),
            )
            .await
            .unwrap();
    }
    let storage = InventoryProbe::new(local);
    let plan = prepare_full_deletion(
        &storage,
        &master,
        &[target(&manifest)],
        Uuid::new_v4(),
        "fixture",
    )
    .await
    .unwrap();
    assert_eq!(plan.metadata.get(&unknown), Some(&19));
    assert!(!plan.metadata.contains_key(&outside));
    storage.assert_read_only();
    // An unrecognized nested object still cannot acquire deletion capability.
    storage
        .inner
        .put_small_versioned(
            Bytes::from_static(b"unapproved nested object"),
            &RemoteKey::new(format!("{prefix}/unexpected/child")),
        )
        .await
        .unwrap();
    assert!(prepare_full_deletion(
        &storage,
        &master,
        &[target(&manifest)],
        Uuid::new_v4(),
        "fixture",
    )
    .await
    .is_err());
    storage.assert_read_only();
    std::fs::remove_dir_all(root).unwrap();
}
