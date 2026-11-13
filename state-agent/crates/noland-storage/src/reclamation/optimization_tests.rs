use super::{
    tests::{bundle, fixture, pack, target},
    *,
};
use crate::{
    DeletionProgress, LocalStorage, PrevalidatedDeletion, RemoteMeta, StorageOperationMetrics,
};
use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex,
    },
    time::Duration,
};

struct CountingStorage {
    inner: LocalStorage,
    stats: AtomicUsize,
    deletes: Mutex<BTreeMap<String, usize>>,
    events: Mutex<Vec<String>>,
    active: AtomicUsize,
    peak: AtomicUsize,
    reappear: Mutex<Option<String>>,
    absent_stats: AtomicUsize,
    catalog_checks: AtomicUsize,
    uncertain_delete: AtomicBool,
}

impl CountingStorage {
    fn new(inner: LocalStorage) -> Self {
        Self {
            inner,
            stats: AtomicUsize::new(0),
            deletes: Mutex::new(BTreeMap::new()),
            events: Mutex::new(Vec::new()),
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            reappear: Mutex::new(None),
            absent_stats: AtomicUsize::new(0),
            catalog_checks: AtomicUsize::new(0),
            uncertain_delete: AtomicBool::new(false),
        }
    }
}

#[async_trait::async_trait]
impl SharedStorageProvider for CountingStorage {
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
    async fn list_prefix(&self, key: &RemoteKey) -> Result<Vec<crate::RemoteEntry>> {
        if key.as_str() == "deletions/bundles" {
            self.catalog_checks.fetch_add(1, Ordering::SeqCst);
        }
        self.inner.list_prefix(key).await
    }
    async fn put_small_versioned(&self, bytes: Bytes, key: &RemoteKey) -> Result<RemoteMeta> {
        self.inner.put_small_versioned(bytes, key).await
    }
    async fn remove_empty_bundle(&self, app: &AppId, id: Uuid) -> Result<()> {
        self.inner.remove_empty_bundle(app, id).await
    }
    async fn delete_object(&self, key: &RemoteKey) -> Result<()> {
        self.inner.delete_object(key).await
    }
    async fn delete_prevalidated(&self, object: &PrevalidatedDeletion) -> Result<()> {
        let key = object.key();
        self.events
            .lock()
            .unwrap()
            .push(format!("delete:{}", key.as_str()));
        *self
            .deletes
            .lock()
            .unwrap()
            .entry(key.as_str().into())
            .or_default() += 1;
        self.inner.delete_object(key).await?;
        if key.as_str().starts_with("packs/") && self.uncertain_delete.swap(false, Ordering::SeqCst)
        {
            return Err(StateError::Storage(
                "fixture lost delete response; cloud outcome is unknown".into(),
            ));
        }
        Ok(())
    }
    async fn stat_for_deletion(&self, key: &RemoteKey) -> Result<Option<RemoteMeta>> {
        self.stats.fetch_add(1, Ordering::SeqCst);
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(active, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(1)).await;
        self.active.fetch_sub(1, Ordering::SeqCst);
        self.events
            .lock()
            .unwrap()
            .push(format!("stat:{}", key.as_str()));
        let result = self.inner.stat(key).await?;
        if result.is_none()
            && self.reappear.lock().unwrap().as_deref() == Some(key.as_str())
            && self.absent_stats.fetch_add(1, Ordering::SeqCst) > 0
        {
            return Ok(Some(RemoteMeta {
                key: key.clone(),
                size: 1,
            }));
        }
        Ok(result)
    }
    fn operation_metrics(&self) -> StorageOperationMetrics {
        let stats = self.stats.load(Ordering::SeqCst) as u64;
        let deletes = self.deletes.lock().unwrap().values().sum::<usize>() as u64;
        StorageOperationMetrics {
            rclone_invocations: stats + deletes,
            remote_stat_calls: stats,
            ..StorageOperationMetrics::default()
        }
    }
}

#[derive(Default)]
struct Samples(Mutex<Vec<DeletionProgress>>);
impl DeletionObserver for Samples {
    fn progress(&self, sample: &DeletionProgress) -> Result<()> {
        self.0.lock().unwrap().push(sample.clone());
        Ok(())
    }
}

#[tokio::test]
async fn a_115_pack_plan_uses_four_workers_and_deletes_each_exact_key_once() {
    let (root, local, master) = fixture();
    let storage = CountingStorage::new(local);
    let packs = (0..115)
        .map(|id| pack(&root, &master, format!("selected-{id}").as_bytes()))
        .collect::<Vec<_>>();
    let refs = packs.iter().collect::<Vec<_>>();
    let selected = bundle(&storage, &master, "selected", &refs, None).await;
    let keep_pack = pack(&root, &master, b"unrelated payload");
    let keep = bundle(&storage, &master, "unrelated", &[&keep_pack], None).await;
    let plan = prepare_full_deletion(
        &storage,
        &master,
        &[target(&selected)],
        Uuid::new_v4(),
        "fixture",
    )
    .await
    .unwrap();
    let guard = crate::RepositoryGuard::acquire(&storage, &master, plan.operation_id, "fixture")
        .await
        .unwrap();
    let samples = Samples::default();
    execute_full_deletion_observed(&storage, &master, &plan, Some(&samples))
        .await
        .unwrap();
    guard.release().await.unwrap();
    assert_eq!(storage.peak.load(Ordering::SeqCst), 4);
    assert_eq!(storage.active.load(Ordering::SeqCst), 0);
    {
        let deleted = storage.deletes.lock().unwrap();
        assert_eq!(deleted.len(), plan.metadata.len() + 115);
        assert!(
            deleted.values().all(|count| *count == 1),
            "metadata must not be deleted twice"
        );
        let events = storage.events.lock().unwrap();
        let first_delete = events
            .iter()
            .position(|event| event.starts_with("delete:"))
            .unwrap();
        assert_eq!(
            events[..first_delete]
                .iter()
                .filter(|event| event.starts_with("stat:packs/"))
                .count(),
            115,
            "all identity/size checks must finish before the first unlink"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.starts_with("stat:packs/"))
                .count(),
            115 * 3,
            "preflight, immediate absence, and independent final verification are retained"
        );
        let samples = samples.0.lock().unwrap();
        assert!(samples
            .iter()
            .any(|sample| sample.completed_packs > 0 && sample.completed_packs < 115));
        let last = samples.last().unwrap();
        assert_eq!(last.completed_objects, last.total_objects);
        assert_eq!(last.completed_packs, 115);
        assert_eq!(last.verified_absent_bytes, plan.physical_bytes());
        assert!(last.last_success_at.is_some());
        assert!(samples
            .windows(2)
            .all(|s| s[0].completed_objects <= s[1].completed_objects));
    }
    assert!(
        crate::read_committed_manifest(&storage, &master, &keep.app.app_id, keep.bundle_id)
            .await
            .is_ok()
    );
    assert_eq!(
        noland_pack::extract_chunk(
            &storage.inner.root.join(pack_key(&keep_pack.pack_id)),
            &keep_pack.entries[0],
            &master
        )
        .unwrap(),
        b"unrelated payload"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn selection_resolution_reuses_one_authenticated_preview_inventory() {
    let (root, local, master) = fixture();
    let storage = CountingStorage::new(local);
    let p = pack(&root, &master, b"selected");
    let selected = bundle(&storage, &master, "selected", &[&p], None).await;
    crate::update_catalog_with_bundle(&storage, &master, &selected, 0)
        .await
        .unwrap();
    storage.catalog_checks.store(0, Ordering::SeqCst);
    let id = Uuid::new_v4();
    let plan = prepare_full_deletion_selected(
        &storage,
        &master,
        &[(selected.app.app_id.clone(), selected.bundle_id)],
        id,
        "fixture",
    )
    .await
    .unwrap();
    assert_eq!(storage.catalog_checks.load(Ordering::SeqCst), 1);
    let legacy = prepare_full_deletion(&storage, &master, &[target(&selected)], id, "fixture")
        .await
        .unwrap();
    assert_eq!(plan, legacy);
    assert!(prepare_full_deletion_selected(
        &storage,
        &master,
        &[(AppId::desktop("wrong-app"), selected.bundle_id)],
        id,
        "fixture"
    )
    .await
    .is_err());
    assert!(storage.deletes.lock().unwrap().is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn selection_reuse_still_rejects_a_catalog_manifest_commit_mismatch() {
    let (root, local, master) = fixture();
    let storage = CountingStorage::new(local);
    let p = pack(&root, &master, b"selected");
    let mut selected = bundle(&storage, &master, "selected", &[&p], None).await;
    crate::update_catalog_with_bundle(&storage, &master, &selected, 0)
        .await
        .unwrap();
    selected.commit_id = Uuid::new_v4();
    let dir = bundle_dir(&selected.app.app_id, selected.bundle_id);
    storage
        .put_small_versioned(
            Bytes::from(
                seal_metadata(&derive_keys(&master).manifest, b"manifest", &selected)
                    .unwrap()
                    .envelope,
            ),
            &RemoteKey::new(format!("{dir}/manifest.enc")),
        )
        .await
        .unwrap();
    storage
        .put_small_versioned(
            Bytes::from(selected.commit_id.to_string()),
            &RemoteKey::new(format!("{dir}/COMMITTED")),
        )
        .await
        .unwrap();
    let error = prepare_full_deletion_selected(
        &storage,
        &master,
        &[(selected.app.app_id.clone(), selected.bundle_id)],
        Uuid::new_v4(),
        "fixture",
    )
    .await
    .unwrap_err();
    assert!(error
        .to_string()
        .contains("catalog/manifest identity mismatch"));
    assert!(storage.deletes.lock().unwrap().is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn an_uncertain_delete_response_preserves_pending_state_and_reconciles_the_same_plan() {
    let (root, local, master) = fixture();
    let storage = CountingStorage::new(local);
    let p = pack(&root, &master, b"selected");
    let selected = bundle(&storage, &master, "selected", &[&p], None).await;
    let plan = prepare_full_deletion(
        &storage,
        &master,
        &[target(&selected)],
        Uuid::new_v4(),
        "fixture",
    )
    .await
    .unwrap();
    storage.uncertain_delete.store(true, Ordering::SeqCst);
    let guard = crate::RepositoryGuard::acquire(&storage, &master, plan.operation_id, "fixture")
        .await
        .unwrap();
    assert!(execute_full_deletion(&storage, &master, &plan)
        .await
        .unwrap_err()
        .to_string()
        .contains("outcome is unknown"));
    guard.release().await.unwrap();
    assert_eq!(storage.active.load(Ordering::SeqCst), 0);
    assert!(storage
        .inner
        .stat(&pending_key(plan.plan_id))
        .await
        .unwrap()
        .is_some());
    assert!(!completed(&storage, &master, plan.plan_id).await.unwrap());
    assert!(storage
        .inner
        .stat(&RemoteKey::new(pack_key(&p.pack_id)))
        .await
        .unwrap()
        .is_none());
    let guard = crate::RepositoryGuard::acquire(&storage, &master, plan.operation_id, "fixture")
        .await
        .unwrap();
    execute_full_deletion(&storage, &master, &plan)
        .await
        .unwrap();
    guard.release().await.unwrap();
    assert!(completed(&storage, &master, plan.plan_id).await.unwrap());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn final_verification_rejects_reappearance_and_retains_the_recoverable_plan() {
    let (root, local, master) = fixture();
    let storage = CountingStorage::new(local);
    let p = pack(&root, &master, b"selected");
    let selected = bundle(&storage, &master, "selected", &[&p], None).await;
    let plan = prepare_full_deletion(
        &storage,
        &master,
        &[target(&selected)],
        Uuid::new_v4(),
        "fixture",
    )
    .await
    .unwrap();
    *storage.reappear.lock().unwrap() = Some(pack_key(&p.pack_id));
    let guard = crate::RepositoryGuard::acquire(&storage, &master, plan.operation_id, "fixture")
        .await
        .unwrap();
    assert!(execute_full_deletion(&storage, &master, &plan)
        .await
        .is_err());
    guard.release().await.unwrap();
    assert!(!completed(&storage, &master, plan.plan_id).await.unwrap());
    assert!(storage
        .inner
        .stat(&pending_key(plan.plan_id))
        .await
        .unwrap()
        .is_some());
    *storage.reappear.lock().unwrap() = None;
    let guard = crate::RepositoryGuard::acquire(&storage, &master, plan.operation_id, "fixture")
        .await
        .unwrap();
    execute_full_deletion(&storage, &master, &plan)
        .await
        .unwrap();
    guard.release().await.unwrap();
    assert!(completed(&storage, &master, plan.plan_id).await.unwrap());
    std::fs::remove_dir_all(root).unwrap();
}
