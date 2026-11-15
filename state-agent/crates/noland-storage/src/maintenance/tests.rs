use super::*;
use crate::{FullDeletionPlan, Health, RemoteEntry, RemoteMeta};
use async_trait::async_trait;
use serde_json::json;
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
};
use tokio::sync::Barrier;

#[derive(Clone, Copy, Default)]
enum ActivityListing {
    #[default]
    Complete,
    Duplicate,
    Hidden,
    OutsideRoot,
    UnknownDirectory,
}

#[derive(Default)]
struct Data {
    objects: BTreeMap<String, Bytes>,
    writes: Vec<String>,
    deletes: Vec<String>,
    stats: Vec<String>,
    listings: Vec<String>,
    activity_listing: ActivityListing,
    acknowledge_delete_without_removal: bool,
    fail_post_delete_stat: bool,
}

#[derive(Default)]
struct Provider {
    data: Mutex<Data>,
    claim_barrier: Option<Barrier>,
    listing_barrier: Option<Barrier>,
    activity_lists: AtomicUsize,
    pending_after_claim: Mutex<Option<Vec<(RemoteKey, Bytes)>>>,
}

impl Provider {
    fn racing() -> Self {
        Self {
            claim_barrier: Some(Barrier::new(2)),
            listing_barrier: Some(Barrier::new(2)),
            ..Self::default()
        }
    }

    fn insert(&self, key: &RemoteKey, bytes: impl Into<Bytes>) {
        self.data
            .lock()
            .unwrap()
            .objects
            .insert(key.as_str().into(), bytes.into());
    }

    fn sealed<T: Serialize>(&self, master: &MasterKey, key: &RemoteKey, aad: &[u8], value: &T) {
        self.insert(
            key,
            seal_metadata(&derive_keys(master).catalog, aad, value)
                .unwrap()
                .envelope,
        );
    }

    fn activities(&self, master: &MasterKey) -> Vec<RepositoryActivity> {
        self.data
            .lock()
            .unwrap()
            .objects
            .iter()
            .filter(|(name, _)| name.starts_with("maintenance/activity/"))
            .map(|(_, bytes)| {
                open_metadata(
                    &derive_keys(master).catalog,
                    b"repository-activity-v1",
                    bytes.to_vec(),
                )
                .unwrap()
            })
            .collect()
    }
}

#[async_trait]
impl SharedStorageProvider for Provider {
    async fn health_check(&self) -> Result<Health> {
        Ok(Health {
            ok: true,
            provider: "admission-fixture".into(),
            detail: String::new(),
        })
    }

    async fn ensure_root(&self) -> Result<()> {
        Ok(())
    }

    async fn stat(&self, key: &RemoteKey) -> Result<Option<RemoteMeta>> {
        let mut data = self.data.lock().unwrap();
        data.stats.push(key.as_str().into());
        if data.fail_post_delete_stat && data.deletes.iter().any(|deleted| deleted == key.as_str())
        {
            return Err(StateError::Storage(
                "fixture post-delete stat failed".into(),
            ));
        }
        Ok(data.objects.get(key.as_str()).map(|bytes| RemoteMeta {
            key: key.clone(),
            size: bytes.len() as u64,
        }))
    }

    async fn upload_immutable(&self, _local: &Path, _key: &RemoteKey) -> Result<RemoteMeta> {
        Err(StateError::Storage("unexpected immutable upload".into()))
    }

    async fn download(&self, key: &RemoteKey, dest: &Path) -> Result<()> {
        std::fs::write(
            dest,
            self.read_small(key, crate::MAX_SMALL_READ_BYTES).await?,
        )?;
        Ok(())
    }

    async fn read_small(&self, key: &RemoteKey, limit: u64) -> Result<Vec<u8>> {
        let data = self.data.lock().unwrap();
        let bytes = data
            .objects
            .get(key.as_str())
            .ok_or_else(|| StateError::NotFound("fixture object absent".into()))?;
        if bytes.len() as u64 > limit {
            return Err(StateError::Integrity("fixture read exceeds bound".into()));
        }
        Ok(bytes.to_vec())
    }

    async fn list_prefix(&self, prefix: &RemoteKey) -> Result<Vec<RemoteEntry>> {
        let mut entries = {
            let mut data = self.data.lock().unwrap();
            data.listings.push(prefix.as_str().into());
            let mut entries = data
                .objects
                .iter()
                .filter(|(name, _)| name.starts_with(&format!("{}/", prefix.as_str())))
                .map(|(name, bytes)| RemoteEntry {
                    key: RemoteKey::new(name),
                    size: bytes.len() as u64,
                    is_prefix: false,
                })
                .collect::<Vec<_>>();
            if prefix.as_str() == "maintenance/activity" {
                match data.activity_listing {
                    ActivityListing::Complete => {}
                    ActivityListing::Duplicate => {
                        if let Some(entry) = entries.first().cloned() {
                            entries.push(entry);
                        }
                    }
                    ActivityListing::Hidden => entries.clear(),
                    ActivityListing::OutsideRoot => entries.push(RemoteEntry {
                        key: RemoteKey::new(format!(
                            "maintenance/activity-sibling/{}.enc",
                            Uuid::new_v4()
                        )),
                        size: 1,
                        is_prefix: false,
                    }),
                    ActivityListing::UnknownDirectory => entries.push(RemoteEntry {
                        key: RemoteKey::new("maintenance/activity/unknown"),
                        size: 0,
                        is_prefix: true,
                    }),
                }
            }
            entries
        };
        if prefix.as_str() == "maintenance/activity"
            && self.activity_lists.fetch_add(1, Ordering::SeqCst) < 2
        {
            if let Some(barrier) = &self.listing_barrier {
                // Both callers receive the same complete post-publication
                // snapshot before either rejected caller can remove its claim.
                barrier.wait().await;
            }
        }
        entries.sort_by(|a, b| a.key.as_str().cmp(b.key.as_str()));
        Ok(entries)
    }

    async fn put_small_versioned(&self, bytes: Bytes, key: &RemoteKey) -> Result<RemoteMeta> {
        let meta = RemoteMeta {
            key: key.clone(),
            size: bytes.len() as u64,
        };
        {
            let mut data = self.data.lock().unwrap();
            data.writes.push(key.as_str().into());
            assert!(data.objects.insert(key.as_str().into(), bytes).is_none());
        }
        if key.as_str().starts_with("maintenance/activity/") {
            let pending = self.pending_after_claim.lock().unwrap().take();
            if let Some(pending) = pending {
                for (key, bytes) in pending {
                    self.insert(&key, bytes);
                }
            }
            if let Some(barrier) = &self.claim_barrier {
                barrier.wait().await;
            }
        }
        Ok(meta)
    }

    async fn delete_object(&self, key: &RemoteKey) -> Result<()> {
        assert!(key.as_str().starts_with("maintenance/activity/"));
        let mut data = self.data.lock().unwrap();
        data.deletes.push(key.as_str().into());
        if !data.acknowledge_delete_without_removal {
            data.objects.remove(key.as_str());
        }
        Ok(())
    }
}

async fn reader<'a>(
    provider: &'a dyn SharedStorageProvider,
    master: &'a MasterKey,
    owner: &str,
    instance: Uuid,
) -> Result<RepositoryGuard<'a>> {
    RepositoryGuard::acquire_for_instance(
        provider,
        master,
        Uuid::new_v4(),
        owner,
        instance,
        RepositoryAccess::RestoreRead,
    )
    .await
}

#[tokio::test]
async fn concurrent_readers_on_two_instances_are_admitted_in_the_legacy_activity_root() {
    let provider = Provider::racing();
    let master = MasterKey::generate();
    let instance_a = Uuid::new_v4();
    let instance_b = Uuid::new_v4();
    let (a, b) = tokio::join!(
        reader(&provider, &master, "PC A", instance_a),
        reader(&provider, &master, "PC B", instance_b),
    );
    let a = a.unwrap();
    let b = b.unwrap();
    let activities = provider.activities(&master);
    assert_eq!(activities.len(), 2);
    assert_ne!(activities[0].claim_id, activities[1].claim_id);
    assert!(activities.iter().all(|activity| {
        activity.access == RepositoryAccess::RestoreRead
            && [Some(instance_a), Some(instance_b)].contains(&activity.instance_id)
    }));
    {
        let data = provider.data.lock().unwrap();
        assert!(data
            .writes
            .iter()
            .all(|name| name.starts_with("maintenance/activity/")));
        assert!(data
            .listings
            .iter()
            .all(|name| ["maintenance/activity", "maintenance/pending"].contains(&name.as_str())));
    }
    a.release().await.unwrap();
    assert_eq!(provider.activities(&master).len(), 1);
    b.release().await.unwrap();
    assert!(provider.activities(&master).is_empty());
}

#[tokio::test]
async fn same_instance_or_same_owner_cannot_reenter_an_active_or_interrupted_restore() {
    let provider = Provider::default();
    let master = MasterKey::generate();
    let instance = Uuid::new_v4();
    let first = reader(&provider, &master, "PC A", instance).await.unwrap();
    let original = provider.activities(&master);
    let mut first = Some(first);
    for interrupted in [false, true] {
        if interrupted {
            // There is deliberately no implicit remote unlock on cancellation.
            drop(first.take());
        }
        assert!(reader(&provider, &master, "another owner", instance)
            .await
            .is_err());
        assert!(reader(&provider, &master, "PC A", Uuid::new_v4())
            .await
            .is_err());
        assert_eq!(provider.activities(&master), original);
    }
}

#[tokio::test]
async fn simultaneous_same_instance_readers_cannot_both_enter() {
    let provider = Provider::racing();
    let master = MasterKey::generate();
    let instance = Uuid::new_v4();
    let (a, b) = tokio::join!(
        reader(&provider, &master, "PC A", instance),
        reader(&provider, &master, "PC B", instance),
    );
    assert!(a.is_err() && b.is_err());
    assert!(provider.activities(&master).is_empty());
}

#[tokio::test]
async fn interrupted_reader_allows_another_instance_but_still_blocks_exclusive_work() {
    let provider = Provider::default();
    let master = MasterKey::generate();
    let operation = Uuid::new_v4();
    let first = RepositoryGuard::acquire_for_instance(
        &provider,
        &master,
        operation,
        "PC A",
        Uuid::new_v4(),
        RepositoryAccess::RestoreRead,
    )
    .await
    .unwrap();
    let original = provider.activities(&master);
    drop(first);
    let next = RepositoryGuard::acquire_for_instance(
        &provider,
        &master,
        operation,
        "PC B",
        Uuid::new_v4(),
        RepositoryAccess::RestoreRead,
    )
    .await
    .unwrap();
    assert_eq!(provider.activities(&master).len(), 2);
    assert!(
        RepositoryGuard::acquire(&provider, &master, operation, "writer")
            .await
            .is_err()
    );
    next.release().await.unwrap();
    assert_eq!(provider.activities(&master), original);
    assert!(!provider
        .data
        .lock()
        .unwrap()
        .deletes
        .contains(&key(original[0].claim_id).0));
}

#[tokio::test]
async fn readers_and_exclusive_writers_block_each_other_in_both_orders() {
    for reader_first in [false, true] {
        let provider = Provider::default();
        let master = MasterKey::generate();
        let first = if reader_first {
            reader(&provider, &master, "PC A", Uuid::new_v4())
                .await
                .unwrap()
        } else {
            RepositoryGuard::acquire(&provider, &master, Uuid::new_v4(), "writer")
                .await
                .unwrap()
        };
        let original = provider.activities(&master);
        let competing = if reader_first {
            RepositoryGuard::acquire_for_instance(
                &provider,
                &master,
                Uuid::new_v4(),
                "writer",
                Uuid::new_v4(),
                RepositoryAccess::Exclusive,
            )
            .await
        } else {
            reader(&provider, &master, "PC B", Uuid::new_v4()).await
        };
        assert!(competing.is_err());
        assert_eq!(provider.activities(&master), original);
        first.release().await.unwrap();
    }
}

#[tokio::test]
async fn racing_reader_and_exclusive_writer_never_both_enter_in_either_order() {
    for reader_first in [false, true] {
        let provider = Provider::racing();
        let master = MasterKey::generate();
        let read = reader(&provider, &master, "reader", Uuid::new_v4());
        let write = RepositoryGuard::acquire(&provider, &master, Uuid::new_v4(), "writer");
        let (a, b) = if reader_first {
            tokio::join!(read, write)
        } else {
            let (write, read) = tokio::join!(write, read);
            (read, write)
        };
        assert!(a.is_err() && b.is_err());
        assert!(provider.activities(&master).is_empty());
    }
}

#[tokio::test]
async fn legacy_claims_and_recovery_fences_block_all_admission() {
    for owner in ["legacy PC", "lock-recovery:interrupted"] {
        let provider = Provider::default();
        let master = MasterKey::generate();
        let id = Uuid::new_v4();
        let old = json!({
            "claim_id": id,
            "operation_id": Uuid::new_v4(),
            "owner": owner,
        });
        provider.sealed(&master, &key(id), b"repository-activity-v1", &old);
        let original = repository_activities(&provider, &master).await.unwrap();
        assert_eq!(original[0].access, RepositoryAccess::Exclusive);
        assert_eq!(original[0].instance_id, None);
        assert!(reader(&provider, &master, "new PC", Uuid::new_v4())
            .await
            .is_err());
        assert!(
            RepositoryGuard::acquire(&provider, &master, Uuid::new_v4(), "writer")
                .await
                .is_err()
        );
        assert_eq!(provider.activities(&master), original);
    }
}

fn pending_objects(master: &MasterKey, operation: Uuid) -> Vec<(RemoteKey, Bytes)> {
    let id = Uuid::new_v4();
    let bundle = Uuid::new_v4();
    let plan: FullDeletionPlan = serde_json::from_value(json!({
        "plan_id": id,
        "operation_id": operation,
        "owner": "deletion owner",
        "automatic": false,
        "targets": [{
            "deletion": crate::BundleDeletion {
                app_id: noland_state_core::AppId::desktop("fixture"),
                bundle_id: bundle,
                commit_id: Uuid::new_v4(),
            },
            "name": "fixture",
            "logical_bytes": 0,
        }],
        "requested_count": 1,
        "packs": {},
        "metadata": {},
        "target_hashes": { (bundle.to_string()): ["manifest", "index"] },
        "inventory_hash": "inventory",
    }))
    .unwrap();
    [
        (
            RemoteKey::new(format!("maintenance/pending/{id}.enc")),
            b"pending-deletion-v1".as_slice(),
            serde_json::to_value(id).unwrap(),
        ),
        (
            RemoteKey::new(format!("deletions/plans/{id}.enc")),
            b"full-deletion-plan-v1".as_slice(),
            serde_json::to_value(plan).unwrap(),
        ),
    ]
    .into_iter()
    .map(|(key, aad, value)| {
        (
            key,
            Bytes::from(
                seal_metadata(&derive_keys(master).catalog, aad, &value)
                    .unwrap()
                    .envelope,
            ),
        )
    })
    .collect()
}

#[tokio::test]
async fn pending_deletion_blocks_readers_before_and_after_publication_even_for_same_operation() {
    for raced in [false, true] {
        for same_operation in [false, true] {
            let provider = Provider::default();
            let master = MasterKey::generate();
            let operation = Uuid::new_v4();
            let pending = pending_objects(
                &master,
                if same_operation {
                    operation
                } else {
                    Uuid::new_v4()
                },
            );
            if raced {
                *provider.pending_after_claim.lock().unwrap() = Some(pending.clone());
            } else {
                for (key, bytes) in &pending {
                    provider.insert(key, bytes.clone());
                }
            }
            assert!(RepositoryGuard::acquire_for_instance(
                &provider,
                &master,
                operation,
                "reader",
                Uuid::new_v4(),
                RepositoryAccess::RestoreRead,
            )
            .await
            .is_err());
            assert!(provider.activities(&master).is_empty());
            let data = provider.data.lock().unwrap();
            assert_eq!(data.writes.len(), usize::from(raced));
            assert_eq!(data.deletes.len(), usize::from(raced));
            for (key, bytes) in pending {
                assert_eq!(data.objects.get(key.as_str()), Some(&bytes));
            }
        }
    }
}

#[tokio::test]
async fn exact_pending_operation_can_still_resume_with_exclusive_admission() {
    let provider = Provider::default();
    let master = MasterKey::generate();
    let operation = Uuid::new_v4();
    for (key, bytes) in pending_objects(&master, operation) {
        provider.insert(&key, bytes);
    }
    assert!(
        RepositoryGuard::acquire(&provider, &master, Uuid::new_v4(), "other")
            .await
            .is_err()
    );
    let guard = RepositoryGuard::acquire(&provider, &master, operation, "deletion owner")
        .await
        .unwrap();
    assert_eq!(
        provider.activities(&master)[0].access,
        RepositoryAccess::Exclusive
    );
    guard.release().await.unwrap();
}

#[tokio::test]
async fn unknown_or_malformed_scope_and_identity_never_grant_reader_admission() {
    for mutation in 0..15 {
        let provider = Provider::default();
        let master = MasterKey::generate();
        let id = Uuid::new_v4();
        let mut value = json!({
            "claim_id": id,
            "operation_id": Uuid::new_v4(),
            "owner": "foreign reader",
            "access": "restore_read",
            "instance_id": Uuid::new_v4(),
        });
        match mutation {
            0 => value["access"] = json!("unknown_read"),
            1 => value["access"] = json!(null),
            2 => value["access"] = json!({ "kind": "restore_read" }),
            3 => {
                value.as_object_mut().unwrap().remove("instance_id");
            }
            4 => value["instance_id"] = json!(null),
            5 => value["instance_id"] = json!(Uuid::nil()),
            6 => value["instance_id"] = json!("not-an-instance"),
            7 => value["instance_id"] = json!(42),
            8 => value["claim_id"] = json!(Uuid::new_v4()),
            9 => value["operation_id"] = json!(Uuid::nil()),
            10 => value["owner"] = json!(" "),
            11 => value["owner"] = json!("lock-recovery:cannot-be-a-reader"),
            12 => value["scope"] = json!("unknown future scope"),
            13 => value["access"] = json!("RestoreRead"),
            _ => value["claim_id"] = json!(Uuid::nil()),
        }
        provider.sealed(&master, &key(id), b"repository-activity-v1", &value);
        let original = provider
            .data
            .lock()
            .unwrap()
            .objects
            .get(key(id).as_str())
            .cloned();
        assert!(
            reader(&provider, &master, "new PC", Uuid::new_v4())
                .await
                .is_err(),
            "mutation {mutation}"
        );
        assert!(
            RepositoryGuard::acquire(&provider, &master, Uuid::new_v4(), "writer")
                .await
                .is_err(),
            "mutation {mutation}"
        );
        assert_eq!(
            provider
                .data
                .lock()
                .unwrap()
                .objects
                .get(key(id).as_str())
                .cloned(),
            original,
            "mutation {mutation}",
        );
    }
}

#[tokio::test]
async fn invalid_local_instance_scope_fails_before_any_claim_is_written() {
    let provider = Provider::default();
    let master = MasterKey::generate();
    for access in [RepositoryAccess::RestoreRead, RepositoryAccess::Exclusive] {
        assert!(RepositoryGuard::acquire_for_instance(
            &provider,
            &master,
            Uuid::new_v4(),
            "PC",
            Uuid::nil(),
            access,
        )
        .await
        .is_err());
    }
    assert!(provider.data.lock().unwrap().writes.is_empty());
}

#[tokio::test]
async fn malformed_unauthenticated_and_ambiguous_inventory_is_not_filtered_into_admission() {
    for mutation in 0..8 {
        let provider = Provider::default();
        let master = MasterKey::generate();
        let id = Uuid::new_v4();
        match mutation {
            0 => provider.data.lock().unwrap().activity_listing = ActivityListing::Duplicate,
            1 => provider.data.lock().unwrap().activity_listing = ActivityListing::Hidden,
            2 => provider.data.lock().unwrap().activity_listing = ActivityListing::OutsideRoot,
            3 => provider.data.lock().unwrap().activity_listing = ActivityListing::UnknownDirectory,
            4 => provider.insert(&key(id), Bytes::from_static(b"not authenticated")),
            5 => provider.insert(
                &RemoteKey::new("maintenance/activity/unknown.tmp"),
                Bytes::from_static(b"never filter unexpected metadata"),
            ),
            6 => {
                let activity = json!({
                    "claim_id": id,
                    "operation_id": Uuid::new_v4(),
                    "owner": "foreign",
                    "access": "restore_read",
                    "instance_id": Uuid::new_v4(),
                });
                provider.sealed(
                    &master,
                    &RemoteKey::new(format!("maintenance/activity/nested/{id}.enc")),
                    b"repository-activity-v1",
                    &activity,
                );
            }
            _ => {
                provider.sealed(&master, &key(id), b"wrong activity AAD", &json!({}));
            }
        }
        assert!(
            reader(&provider, &master, "reader", Uuid::new_v4())
                .await
                .is_err(),
            "mutation {mutation}"
        );
        assert!(provider
            .data
            .lock()
            .unwrap()
            .deletes
            .iter()
            .all(|deleted| deleted != key(id).as_str()));
    }
}

#[test]
fn old_json_is_exclusive_and_legacy_clients_still_deserialize_new_reader_claims() {
    #[derive(Deserialize)]
    struct LegacyActivity {
        claim_id: Uuid,
        operation_id: Uuid,
        owner: String,
    }
    let old = json!({
        "claim_id": Uuid::new_v4(),
        "operation_id": Uuid::new_v4(),
        "owner": "legacy PC",
    });
    let mut activity: RepositoryActivity = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(activity.access, RepositoryAccess::Exclusive);
    assert_eq!(activity.instance_id, None);
    assert_eq!(serde_json::to_value(&activity).unwrap(), old);
    activity.access = RepositoryAccess::RestoreRead;
    activity.instance_id = Some(Uuid::new_v4());
    let legacy: LegacyActivity =
        serde_json::from_value(serde_json::to_value(&activity).unwrap()).unwrap();
    assert_eq!(legacy.claim_id, activity.claim_id);
    assert_eq!(legacy.operation_id, activity.operation_id);
    assert_eq!(legacy.owner, activity.owner);
}

#[tokio::test]
async fn exact_clear_authenticates_access_and_instance_before_removing_any_claim() {
    let provider = Provider::default();
    let master = MasterKey::generate();
    let guard = reader(&provider, &master, "PC A", Uuid::new_v4())
        .await
        .unwrap();
    for change_access in [false, true] {
        let mut wrong = guard.activity.clone();
        if change_access {
            wrong.access = RepositoryAccess::Exclusive;
        } else {
            wrong.instance_id = Some(Uuid::new_v4());
        }
        assert!(clear_repository_activity(&provider, &master, &wrong)
            .await
            .is_err());
        assert!(provider.data.lock().unwrap().deletes.is_empty());
    }
    guard.release().await.unwrap();
}

#[tokio::test]
async fn release_rejects_an_acknowledged_delete_without_confirmed_absence() {
    let provider = Provider::default();
    let master = MasterKey::generate();
    let first = reader(&provider, &master, "PC A", Uuid::new_v4())
        .await
        .unwrap();
    let second = reader(&provider, &master, "PC B", Uuid::new_v4())
        .await
        .unwrap();
    let claim_key = key(first.activity.claim_id);
    let original = provider.data.lock().unwrap().objects.clone();
    provider
        .data
        .lock()
        .unwrap()
        .acknowledge_delete_without_removal = true;
    let error = first.release().await.err().unwrap();
    assert!(matches!(error, StateError::Conflict(_)));
    assert!(error.to_string().contains("cleanup"));
    let data = provider.data.lock().unwrap();
    assert_eq!(data.deletes.as_slice(), std::slice::from_ref(&claim_key.0));
    assert_eq!(data.stats, [claim_key.0.clone(), claim_key.0]);
    assert_eq!(data.objects, original);
    drop(data);
    drop(second);
}

#[tokio::test]
async fn successful_clear_verifies_absence_and_an_already_absent_claim_never_redeletes() {
    let provider = Provider::default();
    let master = MasterKey::generate();
    let guard = reader(&provider, &master, "PC A", Uuid::new_v4())
        .await
        .unwrap();
    let activity = guard.activity.clone();
    let claim_key = key(activity.claim_id);
    guard.release().await.unwrap();
    {
        let data = provider.data.lock().unwrap();
        assert_eq!(data.deletes.as_slice(), std::slice::from_ref(&claim_key.0));
        assert_eq!(data.stats, [claim_key.0.clone(), claim_key.0.clone()]);
        assert!(!data.objects.contains_key(claim_key.as_str()));
    }
    clear_repository_activity(&provider, &master, &activity)
        .await
        .unwrap();
    let data = provider.data.lock().unwrap();
    assert_eq!(data.deletes.as_slice(), std::slice::from_ref(&claim_key.0));
    assert_eq!(
        data.stats,
        [claim_key.0.clone(), claim_key.0.clone(), claim_key.0]
    );
}

#[tokio::test]
async fn post_delete_stat_failure_is_uncertain_cleanup_and_never_retries_the_delete() {
    let provider = Provider::default();
    let master = MasterKey::generate();
    let guard = reader(&provider, &master, "PC A", Uuid::new_v4())
        .await
        .unwrap();
    let claim_key = key(guard.activity.claim_id);
    provider.data.lock().unwrap().fail_post_delete_stat = true;
    assert!(matches!(guard.release().await, Err(StateError::Storage(_))));
    let data = provider.data.lock().unwrap();
    assert_eq!(data.deletes.as_slice(), std::slice::from_ref(&claim_key.0));
    assert_eq!(data.stats, [claim_key.0.clone(), claim_key.0]);
}

#[tokio::test]
async fn malformed_or_changed_authenticated_claims_never_issue_a_delete_on_release() {
    for mutation in 0..9 {
        let provider = Provider::default();
        let master = MasterKey::generate();
        let guard = reader(&provider, &master, "PC A", Uuid::new_v4())
            .await
            .unwrap();
        let claim_key = key(guard.activity.claim_id);
        let mut current = serde_json::to_value(&guard.activity).unwrap();
        match mutation {
            0 => current["access"] = json!("unknown"),
            1 => current["access"] = json!("exclusive"),
            2 => current["instance_id"] = json!(Uuid::new_v4()),
            3 => current["instance_id"] = json!(null),
            4 => current["instance_id"] = json!(Uuid::nil()),
            5 => current["owner"] = json!(" "),
            6 => current["operation_id"] = json!(Uuid::nil()),
            7 => current["claim_id"] = json!(Uuid::new_v4()),
            _ => current["unknown_scope"] = json!("future"),
        }
        provider.sealed(&master, &claim_key, b"repository-activity-v1", &current);
        let original = provider.data.lock().unwrap().objects.clone();
        assert!(guard.release().await.is_err(), "mutation {mutation}");
        let data = provider.data.lock().unwrap();
        assert!(data.deletes.is_empty(), "mutation {mutation}");
        assert_eq!(data.objects, original, "mutation {mutation}");
    }
}

#[tokio::test]
async fn matching_expected_fields_do_not_authorize_clearing_a_malformed_reader() {
    let provider = Provider::default();
    let master = MasterKey::generate();
    let invalid = RepositoryActivity {
        claim_id: Uuid::new_v4(),
        operation_id: Uuid::new_v4(),
        owner: "malformed reader".into(),
        access: RepositoryAccess::RestoreRead,
        instance_id: None,
    };
    let claim_key = key(invalid.claim_id);
    provider.sealed(&master, &claim_key, b"repository-activity-v1", &invalid);
    assert!(clear_repository_activity(&provider, &master, &invalid)
        .await
        .is_err());
    let data = provider.data.lock().unwrap();
    assert!(data.deletes.is_empty());
    assert!(data.objects.contains_key(claim_key.as_str()));
}

#[tokio::test]
async fn readers_download_without_mutating_backup_data_or_clearing_interrupted_foreign_claims() {
    let root = std::env::temp_dir().join(format!("instance-admission-{}", Uuid::new_v4()));
    let provider = crate::LocalStorage::new(root.join("cloud"));
    let master = MasterKey::generate();
    let artifacts = [
        ("packs/keep.pack", b"encrypted backup pack".as_slice()),
        ("bundles/app/keep/manifest.enc", b"encrypted manifest"),
        ("bundles/app/keep/index.enc", b"encrypted index"),
        ("bundles/app/keep/COMMITTED", b"immutable commit"),
        ("catalog/catalog.enc", b"encrypted catalog"),
    ];
    for (name, bytes) in artifacts {
        provider
            .put_small_versioned(Bytes::copy_from_slice(bytes), &RemoteKey::new(name))
            .await
            .unwrap();
    }
    let first = reader(&provider, &master, "PC A", Uuid::new_v4())
        .await
        .unwrap();
    let interrupted = first.activity.clone();
    drop(first);
    let next = reader(&provider, &master, "PC B", Uuid::new_v4())
        .await
        .unwrap();
    let destination = root.join("restored.pack");
    provider
        .download(&RemoteKey::new("packs/keep.pack"), &destination)
        .await
        .unwrap();
    assert_eq!(std::fs::read(destination).unwrap(), artifacts[0].1);
    assert!(
        RepositoryGuard::acquire(&provider, &master, Uuid::new_v4(), "deletion")
            .await
            .is_err()
    );
    next.release().await.unwrap();
    assert_eq!(
        repository_activities(&provider, &master).await.unwrap(),
        [interrupted],
    );
    for (name, bytes) in artifacts {
        assert_eq!(std::fs::read(provider.root.join(name)).unwrap(), bytes);
    }
    std::fs::remove_dir_all(root).unwrap();
}
