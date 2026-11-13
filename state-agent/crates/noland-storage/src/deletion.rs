//! Authenticated, monotonic backup deletion. Old catalog writers and pointer
//! recovery cannot resurrect a deleted backup. Full reclamation is preview-bound
//! and includes every dependent bundle; metadata removal is only an inner step.
use bytes::Bytes;
use noland_crypto::{derive_keys, MasterKey};
use noland_state_core::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    commit::download_metadata, open_metadata, seal_metadata, RemoteKey, SharedStorageProvider,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BundleDeletion {
    pub app_id: AppId,
    pub bundle_id: Uuid,
    pub commit_id: Uuid,
}

#[derive(Serialize, Deserialize)]
struct DeletionReceipt {
    #[serde(flatten)]
    deletion: BundleDeletion,
    #[serde(default)]
    full_deletion_plan_id: Option<Uuid>,
}

fn deletion_key(id: Uuid) -> RemoteKey {
    RemoteKey::new(format!("deletions/bundles/{id}.enc"))
}

pub async fn bundle_is_deleted(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    id: Uuid,
) -> Result<bool> {
    Ok(read_deletion(provider, master, id).await?.is_some())
}

pub(crate) fn validate_deletable_key(key: &RemoteKey) -> Result<()> {
    let parts = key.as_str().split('/').collect::<Vec<_>>();
    if parts.len() == 3 && parts[0] == "packs" {
        if let Some(id) = parts[2].strip_suffix(".pack") {
            if Uuid::parse_str(id)
                .ok()
                .is_some_and(|uuid| uuid.to_string() == id)
                && parts[1] == &id[..2]
            {
                return Ok(());
            }
        }
    }
    if parts.len() == 3 && parts[0] == "maintenance" && matches!(parts[1], "activity" | "pending") {
        if parts[2]
            .strip_suffix(".enc")
            .and_then(|id| {
                Uuid::parse_str(id)
                    .ok()
                    .filter(|uuid| uuid.to_string() == id)
            })
            .is_some()
        {
            return Ok(());
        }
    }
    if parts.len() != 4
        || parts[0] != "bundles"
        || parts[1].is_empty()
        || matches!(parts[1], "." | "..")
        || parts.iter().any(|p| p.contains(['\\', '\0', '\n', '\r']))
        || Uuid::parse_str(parts[2])
            .ok()
            .is_none_or(|id| id.to_string() != parts[2])
        || parts[3].is_empty()
        || matches!(parts[3], "." | "..")
    {
        return Err(StateError::UnsafePath(
            "not an exact backup metadata object".into(),
        ));
    }
    Ok(())
}

pub(crate) async fn read_deletion(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    id: Uuid,
) -> Result<Option<BundleDeletion>> {
    Ok(read_receipt(provider, master, id)
        .await?
        .map(|receipt| receipt.deletion))
}

async fn read_receipt(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    id: Uuid,
) -> Result<Option<DeletionReceipt>> {
    let key = deletion_key(id);
    if provider.stat(&key).await?.is_none() {
        return Ok(None);
    }
    let bytes = download_metadata(provider, &key, 16 * 1024).await?;
    let record: DeletionReceipt =
        open_metadata(&derive_keys(master).catalog, b"bundle-deletion-v1", bytes)?;
    if record.deletion.bundle_id != id {
        return Err(StateError::Integrity("deletion identity mismatch".into()));
    }
    Ok(Some(record))
}

pub(crate) async fn filter_deleted(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    catalog: &mut CatalogDocument,
) -> Result<()> {
    let entries = provider
        .list_prefix(&RemoteKey::new("deletions/bundles"))
        .await?;
    let catalog_ids = catalog
        .apps
        .iter()
        .flat_map(|app| app.bundles.iter().map(|b| b.bundle_id))
        .collect::<std::collections::HashSet<_>>();
    for entry in entries {
        if entry.is_prefix {
            continue;
        }
        let id = entry
            .key
            .as_str()
            .strip_prefix("deletions/bundles/")
            .and_then(|v| v.strip_suffix(".enc"))
            .and_then(|v| Uuid::parse_str(v).ok())
            .ok_or_else(|| StateError::Integrity("invalid deletion object name".into()))?;
        if !catalog_ids.contains(&id) {
            continue;
        }
        let deletion = read_deletion(provider, master, id)
            .await?
            .ok_or_else(|| StateError::Integrity("deletion record disappeared".into()))?;
        for app in &mut catalog.apps {
            if app.app_id == deletion.app_id {
                app.bundles.retain(|bundle| bundle.bundle_id != id);
                if let Some(latest) = app.bundles.iter().max_by_key(|b| b.captured_at) {
                    app.latest_bundle_id = latest.bundle_id;
                }
            }
        }
    }
    catalog.apps.retain(|app| !app.bundles.is_empty());
    catalog
        .heads
        .retain(|id| catalog.apps.iter().any(|app| &app.app_id == id));
    Ok(())
}

pub async fn delete_bundle(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    deletion: &BundleDeletion,
) -> Result<()> {
    if let Some(receipt) = read_receipt(provider, master, deletion.bundle_id).await? {
        if receipt.deletion != *deletion {
            return Err(StateError::Conflict(
                "backup deletion identity changed".into(),
            ));
        }
        let Some(id) = receipt.full_deletion_plan_id else {
            return Err(StateError::Conflict("legacy metadata-only deletion has no retained payload inventory; full removal cannot be claimed".into()));
        };
        if crate::reclamation::completed(provider, master, id).await? {
            return Ok(());
        }
        return Err(StateError::Conflict(format!(
            "full deletion is incomplete; resume confirmed plan {id}"
        )));
    }
    let id = Uuid::new_v4();
    let guard = crate::RepositoryGuard::acquire(provider, master, id, "storage-api").await?;
    let result = async {
        let plan = crate::prepare_full_deletion(
            provider,
            master,
            std::slice::from_ref(deletion),
            id,
            "storage-api",
        )
        .await?;
        if plan.targets.len() != 1 {
            return Err(StateError::Conflict(
                "shared data requires a preview confirming every dependent backup".into(),
            ));
        }
        crate::execute_full_deletion(provider, master, &plan).await
    }
    .await;
    guard.release().await?;
    result
}

pub(crate) async fn persist_bundle_deletion_receipt(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    deletion: &BundleDeletion,
    plan_id: Uuid,
) -> Result<()> {
    if let Some(existing) = read_receipt(provider, master, deletion.bundle_id).await? {
        if existing.deletion != *deletion || existing.full_deletion_plan_id != Some(plan_id) {
            return Err(StateError::Conflict(
                "backup deletion identity changed".into(),
            ));
        }
    } else {
        let manifest =
            crate::read_committed_manifest(provider, master, &deletion.app_id, deletion.bundle_id)
                .await?;
        if manifest.commit_id != deletion.commit_id {
            return Err(StateError::Conflict(
                "backup changed since confirmation".into(),
            ));
        }
        let record = seal_metadata(
            &derive_keys(master).catalog,
            b"bundle-deletion-v1",
            &DeletionReceipt {
                deletion: deletion.clone(),
                full_deletion_plan_id: Some(plan_id),
            },
        )?;
        provider
            .put_small_versioned(
                Bytes::from(record.envelope),
                &deletion_key(deletion.bundle_id),
            )
            .await?;
        if read_deletion(provider, master, deletion.bundle_id)
            .await?
            .as_ref()
            != Some(deletion)
        {
            return Err(StateError::Integrity("deletion receipt not durable".into()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        commit_bundle, load_catalog, read_committed_manifest, update_catalog_with_bundle,
        LocalStorage,
    };

    #[tokio::test]
    async fn deletion_is_idempotent_survives_stale_catalog_and_preserves_shared_packs() {
        let root = std::env::current_dir()
            .unwrap()
            .join(format!("target/delete-cloud-{}", Uuid::new_v4()));
        let provider = LocalStorage::new(&root);
        let master = MasterKey::generate();
        let mut first = BundleManifest::new(
            ManifestApp::from(&AppIdentity::new(AppId::desktop("fixture"), "Fixture")),
            ManifestSource {
                instance_id: Uuid::new_v4(),
                image_id: "fixture".into(),
                captured_at: chrono::Utc::now(),
            },
            BackupMode::CompleteApplication,
        );
        commit_bundle(&provider, &master, &first, &[], None)
            .await
            .unwrap();
        update_catalog_with_bundle(&provider, &master, &first, 0)
            .await
            .unwrap();
        let deletion = BundleDeletion {
            app_id: first.app.app_id.clone(),
            bundle_id: first.bundle_id,
            commit_id: first.commit_id,
        };
        let old_catalog = std::fs::read(root.join("catalog/LATEST")).unwrap();
        first.parent_bundle_id = Some(first.bundle_id);
        first.bundle_id = Uuid::new_v4();
        first.commit_id = Uuid::new_v4();
        commit_bundle(&provider, &master, &first, &[], None)
            .await
            .unwrap();
        update_catalog_with_bundle(&provider, &master, &first, 0)
            .await
            .unwrap();
        let pack = root.join("packs/aa/shared.pack");
        std::fs::create_dir_all(pack.parent().unwrap()).unwrap();
        std::fs::write(&pack, b"shared encrypted data").unwrap();
        assert!(delete_bundle(&provider, &MasterKey::generate(), &deletion)
            .await
            .is_err());
        assert!(
            read_committed_manifest(&provider, &master, &deletion.app_id, deletion.bundle_id)
                .await
                .is_ok()
        );
        delete_bundle(&provider, &master, &deletion).await.unwrap();
        delete_bundle(&provider, &master, &deletion).await.unwrap();
        assert!(
            read_committed_manifest(&provider, &master, &deletion.app_id, deletion.bundle_id)
                .await
                .is_err()
        );
        assert!(
            read_committed_manifest(&provider, &master, &first.app.app_id, first.bundle_id)
                .await
                .is_ok()
        );
        let catalog = load_catalog(&provider, &master).await.unwrap();
        assert_eq!(catalog.apps[0].bundles.len(), 1);
        assert_eq!(catalog.apps[0].latest_bundle_id, first.bundle_id);
        std::fs::write(root.join("catalog/LATEST"), old_catalog).unwrap();
        assert!(load_catalog(&provider, &master)
            .await
            .unwrap()
            .apps
            .is_empty());
        std::fs::remove_file(root.join("catalog/LATEST")).unwrap();
        assert_eq!(
            load_catalog(&provider, &master).await.unwrap().apps[0]
                .bundles
                .len(),
            1
        );
        assert_eq!(std::fs::read(pack).unwrap(), b"shared encrypted data");
        let prefix = root.join(bundle_dir(&deletion.app_id, deletion.bundle_id));
        for name in [COMMITTED_MARKER, "manifest.enc", "manifest.hash"] {
            assert!(!prefix.join(name).exists());
        }
        // Corrupt tombstones must not silently turn back into a live backup.
        std::fs::write(
            root.join(deletion_key(deletion.bundle_id).as_str()),
            b"corrupt",
        )
        .unwrap();
        assert!(load_catalog(&provider, &master).await.is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn deletion_never_accepts_prefixes_or_traversal() {
        for key in [
            "",
            "packs/aa/a.pack",
            "bundles",
            "bundles/game",
            "../manifest.enc",
            "bundles/../00000000-0000-0000-0000-000000000000/manifest.enc",
            "bundles/game/../manifest.enc",
            "catalog/LATEST",
        ] {
            assert!(
                validate_deletable_key(&RemoteKey::new(key)).is_err(),
                "{key}"
            );
        }
    }
}
