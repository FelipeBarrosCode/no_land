use std::path::Path;

use bytes::Bytes;
use noland_crypto::{derive_keys, unwrap_envelope, wrap_envelope, MasterKey};
use noland_state_core::*;
use uuid::Uuid;

use crate::{RemoteKey, SharedStorageProvider};

pub struct CatalogStore {
    pub document: CatalogDocument,
}

pub async fn load_catalog(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
) -> Result<CatalogDocument> {
    let latest = provider.stat(&RemoteKey::new(catalog_latest_key())).await?;
    if latest.is_none() {
        return Ok(CatalogDocument::empty());
    }
    let tmp = std::env::temp_dir().join(format!("noland-catalog-{}", Uuid::new_v4()));
    provider
        .download(&RemoteKey::new(catalog_latest_key()), &tmp)
        .await?;
    let pointer = std::fs::read_to_string(&tmp).unwrap_or_default();
    let _ = std::fs::remove_file(&tmp);
    let commit_id = pointer.trim();
    if commit_id.is_empty() {
        return recover_latest_catalog(provider, master).await;
    }
    match fetch_catalog_commit(provider, master, commit_id).await {
        Ok(doc) => Ok(doc),
        Err(_) => recover_latest_catalog(provider, master).await,
    }
}

async fn fetch_catalog_commit(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    commit_id: &str,
) -> Result<CatalogDocument> {
    let key = RemoteKey::new(format!("catalog/commits/{commit_id}.enc"));
    let tmp = std::env::temp_dir().join(format!("noland-catc-{commit_id}"));
    provider.download(&key, &tmp).await?;
    let bytes = std::fs::read(&tmp)?;
    let _ = std::fs::remove_file(&tmp);
    let keys = derive_keys(master);
    let plain = unwrap_envelope(&keys.catalog, b"catalog", &bytes)?;
    Ok(serde_json::from_slice(&plain)?)
}

async fn recover_latest_catalog(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
) -> Result<CatalogDocument> {
    let entries = provider
        .list_prefix(&RemoteKey::new("catalog/commits"))
        .await
        .unwrap_or_default();
    let mut best: Option<CatalogDocument> = None;
    for entry in entries {
        if !entry.key.as_str().ends_with(".enc") {
            continue;
        }
        let stem = Path::new(entry.key.as_str())
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        if let Ok(doc) = fetch_catalog_commit(provider, master, &stem).await {
            if best
                .as_ref()
                .map(|b| doc.created_at > b.created_at)
                .unwrap_or(true)
            {
                best = Some(doc);
            }
        }
    }
    Ok(best.unwrap_or_else(CatalogDocument::empty))
}

pub async fn commit_bundle(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    manifest: &BundleManifest,
    pack_files: &[(String, std::path::PathBuf)],
    db: Option<&noland_state_db::StateDb>,
) -> Result<()> {
    commit_bundle_with_index(provider, master, manifest, pack_files, None, db).await
}

pub async fn commit_bundle_with_index(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    manifest: &BundleManifest,
    pack_files: &[(String, std::path::PathBuf)],
    pack_index_json: Option<&[u8]>,
    db: Option<&noland_state_db::StateDb>,
) -> Result<()> {
    provider.ensure_root().await?;
    let keys = derive_keys(master);
    let bundle_prefix = bundle_dir(&manifest.app.app_id, manifest.bundle_id);

    for (pack_id, path) in pack_files {
        let key = RemoteKey::new(pack_key(pack_id));
        provider.upload_immutable(path, &key).await?;
        if let Some(db) = db {
            db.journal_put(
                &manifest.commit_id.to_string(),
                key.as_str(),
                "upload",
                "ok",
                None,
            )?;
        }
    }

    if let Some(index) = pack_index_json {
        let enc_index = wrap_envelope(&keys.manifest, b"pack-index", index)?;
        provider
            .put_small_versioned(
                Bytes::from(enc_index),
                &RemoteKey::new(format!("{bundle_prefix}/index.enc")),
            )
            .await?;
    }

    let manifest_json = serde_json::to_vec_pretty(manifest)?;
    let manifest_hash = format!("blake3:{}", blake3::hash(&manifest_json).to_hex());
    let enc = wrap_envelope(&keys.manifest, b"manifest", &manifest_json)?;
    provider
        .put_small_versioned(
            Bytes::from(enc),
            &RemoteKey::new(format!("{bundle_prefix}/manifest.enc")),
        )
        .await?;
    provider
        .put_small_versioned(
            Bytes::from(manifest_hash.clone()),
            &RemoteKey::new(format!("{bundle_prefix}/manifest.hash")),
        )
        .await?;
    // COMMITTED is written last. Incomplete bundles stay invisible.
    provider
        .put_small_versioned(
            Bytes::from(manifest.commit_id.to_string()),
            &RemoteKey::new(format!("{bundle_prefix}/{COMMITTED_MARKER}")),
        )
        .await?;

    if let Some(db) = db {
        db.record_commit(
            manifest.commit_id,
            &manifest.app.app_id,
            manifest.bundle_id,
            &manifest_hash,
            Some(&bundle_prefix),
            CommitVisibility::Committed,
        )?;
    }
    Ok(())
}

pub async fn update_catalog_with_bundle(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    manifest: &BundleManifest,
    stored_incremental_size: u64,
) -> Result<CatalogDocument> {
    let committed = provider
        .stat(&RemoteKey::new(format!(
            "{}/{COMMITTED_MARKER}",
            bundle_dir(&manifest.app.app_id, manifest.bundle_id)
        )))
        .await?;
    if committed.is_none() {
        return Err(StateError::IncompleteCommit);
    }

    let mut catalog = load_catalog(provider, master).await?;
    catalog.catalog_commit_id = Uuid::new_v4();
    catalog.created_at = chrono::Utc::now();
    catalog.upsert_bundle_from_manifest(
        &manifest.app,
        CatalogBundle {
            bundle_id: manifest.bundle_id,
            commit_id: manifest.commit_id,
            parent_bundle_id: manifest.parent_bundle_id,
            captured_at: manifest.source.captured_at,
            source_instance_id: manifest.source.instance_id,
            mode: manifest.mode,
            logical_size: manifest.logical_size(),
            stored_incremental_size,
        },
    );
    write_catalog(provider, master, &catalog).await?;
    Ok(catalog)
}

pub async fn write_catalog(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    catalog: &CatalogDocument,
) -> Result<()> {
    let keys = derive_keys(master);
    let json = serde_json::to_vec_pretty(catalog)?;
    let enc = wrap_envelope(&keys.catalog, b"catalog", &json)?;
    let commit_key = RemoteKey::new(catalog_commit_key(catalog.catalog_commit_id));
    provider
        .put_small_versioned(Bytes::from(enc), &commit_key)
        .await?;
    provider
        .put_small_versioned(
            Bytes::from(COMMITTED_MARKER.as_bytes().to_vec()),
            &RemoteKey::new(format!(
                "catalog/commits/{}.{}",
                catalog.catalog_commit_id, COMMITTED_MARKER
            )),
        )
        .await?;
    // LATEST is a convenience pointer. Restore can recover from catalog history.
    let _ = provider
        .put_small_versioned(
            Bytes::from(catalog.catalog_commit_id.to_string()),
            &RemoteKey::new(catalog_latest_key()),
        )
        .await;
    Ok(())
}

pub async fn read_pack_index(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    app_id: &AppId,
    bundle_id: Uuid,
) -> Result<Vec<noland_pack::PackIndexEntry>> {
    let prefix = bundle_dir(app_id, bundle_id);
    let tmp = std::env::temp_dir().join(format!("noland-idx-{bundle_id}"));
    provider
        .download(&RemoteKey::new(format!("{prefix}/index.enc")), &tmp)
        .await?;
    let bytes = std::fs::read(&tmp)?;
    let _ = std::fs::remove_file(&tmp);
    let keys = derive_keys(master);
    let plain = unwrap_envelope(&keys.manifest, b"pack-index", &bytes)?;
    Ok(serde_json::from_slice(&plain)?)
}

pub async fn read_committed_manifest(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    app_id: &AppId,
    bundle_id: Uuid,
) -> Result<BundleManifest> {
    let prefix = bundle_dir(app_id, bundle_id);
    if provider
        .stat(&RemoteKey::new(format!("{prefix}/{COMMITTED_MARKER}")))
        .await?
        .is_none()
    {
        return Err(StateError::IncompleteCommit);
    }
    let tmp = std::env::temp_dir().join(format!("noland-man-{bundle_id}"));
    provider
        .download(&RemoteKey::new(format!("{prefix}/manifest.enc")), &tmp)
        .await?;
    let bytes = std::fs::read(&tmp)?;
    let _ = std::fs::remove_file(&tmp);
    let keys = derive_keys(master);
    let plain = unwrap_envelope(&keys.manifest, b"manifest", &bytes)?;
    Ok(serde_json::from_slice(&plain)?)
}

pub async fn commit_checkpoint(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    checkpoint: &LearnedStateCheckpoint,
) -> Result<()> {
    let keys = derive_keys(master);
    let dir = checkpoint_dir(checkpoint.instance_id, checkpoint.checkpoint_id);
    let json = serde_json::to_vec_pretty(checkpoint)?;
    let enc = wrap_envelope(&keys.catalog, b"checkpoint", &json)?;
    provider
        .put_small_versioned(
            Bytes::from(enc),
            &RemoteKey::new(format!("{dir}/state.enc")),
        )
        .await?;
    provider
        .put_small_versioned(
            Bytes::from(enc_meta(checkpoint)),
            &RemoteKey::new(format!("{dir}/checkpoint.enc")),
        )
        .await?;
    provider
        .put_small_versioned(
            Bytes::from(checkpoint.checkpoint_id.to_string()),
            &RemoteKey::new(format!("{dir}/{COMMITTED_MARKER}")),
        )
        .await?;
    Ok(())
}

pub async fn commit_seal(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    seal: &SealRecord,
) -> Result<()> {
    let keys = derive_keys(master);
    let dir = seal_dir(seal.instance_id, seal.seal_id);
    let json = serde_json::to_vec_pretty(seal)?;
    let enc = wrap_envelope(&keys.catalog, b"seal", &json)?;
    provider
        .put_small_versioned(Bytes::from(enc), &RemoteKey::new(format!("{dir}/seal.enc")))
        .await?;
    provider
        .put_small_versioned(
            Bytes::from(seal.seal_id.to_string()),
            &RemoteKey::new(format!("{dir}/{COMMITTED_MARKER}")),
        )
        .await?;
    Ok(())
}

fn enc_meta(checkpoint: &LearnedStateCheckpoint) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "checkpoint_id": checkpoint.checkpoint_id,
        "instance_id": checkpoint.instance_id,
        "created_at": checkpoint.created_at,
    }))
    .unwrap_or_default()
}
