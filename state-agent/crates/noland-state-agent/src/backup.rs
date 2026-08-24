use std::path::PathBuf;

use chrono::Utc;
use noland_cas::{blake3_file, chunk_file};
use noland_classifier::Classifier;
use noland_crypto::MasterKey;
use noland_pack::pack_chunks;
use noland_rclone_adapter::EphemeralRcloneSession;
use noland_snapshot::{create_view, discard};
use noland_state_core::*;
use noland_storage::{
    commit_bundle_with_index, shred_ephemeral_session, update_catalog_with_bundle,
    write_ephemeral_session, LocalStorage, RcloneStorage, SharedStorageProvider,
};
use uuid::Uuid;

use crate::reconcile::reconcile_app;
use crate::StateAgent;

pub async fn run_backup(
    agent: &StateAgent,
    app_id: &AppId,
    mode: BackupMode,
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
) -> Result<BundleManifest> {
    let identity = agent
        .db
        .get_app(app_id)?
        .ok_or_else(|| StateError::NotFound(app_id.to_string()))?;
    let op_id = Uuid::new_v4();
    let mut op = OperationRecord {
        operation_id: op_id,
        kind: "backup".into(),
        app_id: Some(app_id.clone()),
        state: BackupState::Discovering.as_str().into(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        last_error: None,
        detail_json: serde_json::json!({}),
    };
    agent.db.upsert_operation(&op)?;

    op.state = BackupState::Reconciling.as_str().into();
    agent.db.upsert_operation(&op)?;
    reconcile_app(agent, app_id)?;

    let classifier = Classifier::new(&agent.db, &agent.config.image_id);
    classifier.reclassify_app(app_id)?;

    let associations = agent.db.associations_for_app(app_id)?;
    let mut include_paths = Vec::new();
    let mut files = Vec::new();
    let roots = agent.roots.lock().clone();

    for (record, assoc) in &associations {
        let decision = classifier.decide(record, assoc, mode)?;
        match decision {
            BackupDecision::Exclude
            | BackupDecision::MetadataOnly
            | BackupDecision::DeferAndReconcile => {
                continue;
            }
            BackupDecision::Include
            | BackupDecision::IncludeSharedReference
            | BackupDecision::IncludeAsOverlay
            | BackupDecision::IncludeAsBaseOrOverlay
            | BackupDecision::EncryptedInclude => {
                let path = PathBuf::from(&record.canonical_path);
                if path.is_file() {
                    include_paths.push(path);
                    files.push((record.clone(), assoc.clone()));
                }
            }
        }
    }

    if let Some((record, association)) = track_steam_appmanifest(agent, &identity, &roots)? {
        let path = PathBuf::from(&record.canonical_path);
        if !include_paths.iter().any(|included| included == &path) {
            include_paths.push(path);
            files.push((record, association));
        }
    }

    op.state = BackupState::Snapshotting.as_str().into();
    agent.db.upsert_operation(&op)?;
    let view = create_view(&agent.config.paths.snapshots, &include_paths, true)?;

    op.state = BackupState::Hashing.as_str().into();
    agent.db.upsert_operation(&op)?;

    let mut manifest = BundleManifest::new(
        ManifestApp::from(&identity),
        ManifestSource {
            instance_id: agent.config.instance_id,
            image_id: agent.config.image_id.clone(),
            captured_at: Utc::now(),
        },
        mode,
    );
    manifest.consistency = view.consistency;
    if let Ok(Some((commit, bundle, _))) = agent.db.latest_commit(app_id) {
        let _ = commit;
        manifest.parent_bundle_id = Some(bundle);
    }

    let mut chunk_payloads = Vec::new();
    for mapping in &view.mappings {
        let Some((record, assoc)) = files
            .iter()
            .find(|(r, _)| PathBuf::from(&r.canonical_path) == mapping.source)
        else {
            continue;
        };
        let logical = roots.classify(&mapping.source).unwrap_or_else(|| {
            LogicalPath::new(LogicalRoot::Home, mapping.source.display().to_string())
        });
        validate_relative_path(&logical.relative_path).ok();
        let chunks = chunk_file(&mapping.staged)?;
        let file_hash = if chunks.file_hash.is_empty() {
            blake3_file(&mapping.staged)?
        } else {
            chunks.file_hash.clone()
        };
        for (meta, payload) in chunks.chunks.iter().zip(chunks.payloads.iter()) {
            if !agent.db.known_chunk(&meta.hash)? {
                chunk_payloads.push((meta.hash.clone(), payload.clone()));
            } else {
                noland_state_core::metrics::Metrics::inc(&agent.metrics.chunks_reused_total);
            }
        }
        noland_state_core::metrics::Metrics::add(&agent.metrics.hash_bytes_total, chunks.size);
        manifest.files.push(ManifestFile {
            logical_root: logical.logical_root.as_token(),
            relative_path: logical.relative_path,
            source_path_hint: Some(record.canonical_path.clone()),
            file_type: record.file_type.clone().unwrap_or_else(|| "file".into()),
            size: chunks.size,
            file_hash,
            chunks: chunks.chunks,
            mode: record.mode.map(|m| m as u32),
            mtime_ns: record.mtime_ns,
            uid: record.uid.map(|u| u as u32),
            gid: record.gid.map(|g| g as u32),
            symlink_target: None,
            persistence_class: assoc.persistence_class,
            semantic_role: assoc.semantic_role,
            association_confidence: assoc.confidence,
            shared_app_ids: agent
                .db
                .associations_for_path(record.path_id)?
                .into_iter()
                .filter(|a| a.app_id != *app_id && a.confidence >= OWNERSHIP_CANDIDATE_MIN)
                .map(|a| a.app_id)
                .collect(),
        });
    }

    op.state = BackupState::Packing.as_str().into();
    agent.db.upsert_operation(&op)?;
    let pack_dir = agent
        .config
        .paths
        .packs
        .join(manifest.bundle_id.to_string());
    let packs = pack_chunks(&pack_dir, master, chunk_payloads, |hash| {
        agent.db.known_chunk(hash).unwrap_or(false)
    })?;
    let mut incremental = 0u64;
    let mut pack_files = Vec::new();
    let mut pack_index = Vec::new();
    for pack in &packs {
        incremental += pack.bytes;
        pack_files.push((pack.pack_id.clone(), pack.path.clone()));
        pack_index.extend(pack.entries.iter().cloned());
        for entry in &pack.entries {
            agent.db.remember_chunk(
                &entry.chunk_hash,
                Some(&pack.pack_id),
                entry.plaintext_len as u64,
            )?;
            noland_state_core::metrics::Metrics::inc(&agent.metrics.chunks_created_total);
        }
        noland_state_core::metrics::Metrics::add(
            &agent.metrics.pack_bytes_created_total,
            pack.bytes,
        );
    }

    op.state = BackupState::Uploading.as_str().into();
    agent.db.upsert_operation(&op)?;
    agent.db.record_commit(
        manifest.commit_id,
        app_id,
        manifest.bundle_id,
        "pending",
        None,
        CommitVisibility::Uploading,
    )?;

    op.state = BackupState::Committing.as_str().into();
    agent.db.upsert_operation(&op)?;
    let index_json = serde_json::to_vec(&pack_index)?;
    commit_bundle_with_index(
        provider,
        master,
        &manifest,
        &pack_files,
        Some(&index_json),
        Some(&agent.db),
    )
    .await?;
    update_catalog_with_bundle(provider, master, &manifest, incremental).await?;

    op.state = BackupState::Checkpointing.as_str().into();
    agent.db.upsert_operation(&op)?;
    let _ = crate::checkpoint::write_local_checkpoint(agent);

    agent.db.clear_dirty(app_id)?;
    discard(&view)?;
    op.state = BackupState::Completed.as_str().into();
    op.updated_at = Utc::now();
    agent.db.upsert_operation(&op)?;
    Ok(manifest)
}

fn track_steam_appmanifest(
    agent: &StateAgent,
    identity: &AppIdentity,
    roots: &LogicalRootMap,
) -> Result<Option<(PathRecord, PathAssociation)>> {
    let Some(path) = steam_appmanifest_path(identity, roots) else {
        return Ok(None);
    };
    let metadata = std::fs::metadata(&path)?;
    let canonical_path = path.to_string_lossy().into_owned();
    let logical = roots
        .classify(&path)
        .ok_or_else(|| StateError::NotFound(format!("logical root for {}", path.display())))?;
    let path_id = agent.db.upsert_path(&canonical_path)?;
    let now = Utc::now();

    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;

    let record = PathRecord {
        path_id,
        canonical_path,
        logical_root: Some(logical.logical_root.as_token()),
        relative_path: Some(logical.relative_path),
        file_type: Some("file".into()),
        #[cfg(unix)]
        inode: Some(metadata.ino() as i64),
        #[cfg(not(unix))]
        inode: None,
        mount_id: None,
        size: Some(metadata.len() as i64),
        mtime_ns: metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|value| value.as_nanos().min(i64::MAX as u128) as i64),
        #[cfg(unix)]
        mode: Some(metadata.mode() as i64),
        #[cfg(not(unix))]
        mode: None,
        #[cfg(unix)]
        uid: Some(metadata.uid() as i64),
        #[cfg(not(unix))]
        uid: None,
        #[cfg(unix)]
        gid: Some(metadata.gid() as i64),
        #[cfg(not(unix))]
        gid: None,
        content_hash: None,
        last_scanned_at: Some(now.timestamp()),
    };
    agent.db.update_path_meta(path_id, &record)?;

    let association = PathAssociation {
        app_id: identity.app_id.clone(),
        path_id,
        confidence: CONF_EXPLICIT,
        evidence: vec![Evidence::new(EvidenceKind::SteamMetadata)],
        persistence_class: PersistenceClass::PersistentState,
        semantic_role: SemanticRole::AppContent,
        first_seen_at: now,
        last_seen_at: now,
    };
    agent.db.upsert_association(&association)?;

    Ok(Some((record, association)))
}

fn steam_appmanifest_path(identity: &AppIdentity, roots: &LogicalRootMap) -> Option<PathBuf> {
    let steam_app_id = identity.steam_app_id.or_else(|| {
        identity
            .app_id
            .as_str()
            .strip_prefix("steam:")
            .and_then(|value| value.parse::<u32>().ok())
    })?;
    let filename = format!("appmanifest_{steam_app_id}.acf");

    roots
        .steam_libraries
        .values()
        .map(|steamapps| steamapps.join(&filename))
        .find(|path| path.is_file())
}

pub async fn run_backup_to_local(
    agent: &StateAgent,
    app_id: &AppId,
    mode: BackupMode,
    cloud_root: PathBuf,
    master: &MasterKey,
) -> Result<BundleManifest> {
    let storage = LocalStorage::new(cloud_root);
    storage.ensure_root().await?;
    run_backup(agent, app_id, mode, &storage, master).await
}

/// Backup using an ephemeral rclone session minted by the desktop adapter.
pub async fn run_backup_with_session(
    agent: &StateAgent,
    app_id: &AppId,
    mode: BackupMode,
    session: &EphemeralRcloneSession,
    master: &MasterKey,
) -> Result<BundleManifest> {
    let config_path = write_ephemeral_session(&agent.config.paths.run_root, session)?;
    let storage = RcloneStorage::from_session(session, &config_path);
    let result = run_backup(agent, app_id, mode, &storage, master).await;
    let _ = shred_ephemeral_session(&agent.config.paths.run_root, &session.operation_id);
    result
}

pub async fn run_backup_all_with_session(
    agent: &StateAgent,
    mode: BackupMode,
    session: &EphemeralRcloneSession,
    master: &MasterKey,
) -> Result<Vec<BundleManifest>> {
    let dirty = agent.db.list_dirty_apps()?;
    let portable: std::collections::HashSet<String> =
        noland_discovery::filter_backup_candidates(agent.db.list_apps()?)
            .into_iter()
            .map(|a| a.app_id.as_str().to_string())
            .collect();
    let mut targets: Vec<AppId> = dirty
        .into_iter()
        .map(|d| d.app_id)
        .filter(|id| portable.contains(id.as_str()))
        .collect();
    if targets.is_empty() {
        targets = portable.into_iter().map(AppId).collect();
    }
    let mut out = Vec::new();
    for app_id in targets {
        match run_backup_with_session(agent, &app_id, mode, session, master).await {
            Ok(manifest) => out.push(manifest),
            Err(StateError::NotFound(_)) => continue,
            Err(err) => return Err(err),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::steam_appmanifest_path;
    use noland_state_core::{AppId, AppIdentity, LogicalRootMap};

    #[test]
    fn finds_manifest_in_registered_steam_library() {
        let root = std::env::temp_dir().join(format!(
            "noland-steam-manifest-tracking-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let steamapps = root.join("steamapps");
        std::fs::create_dir_all(&steamapps).unwrap();
        let manifest = steamapps.join("appmanifest_3241660.acf");
        std::fs::write(&manifest, b"appmanifest").unwrap();

        let mut roots = LogicalRootMap::default();
        roots.steam_libraries.insert("0".into(), steamapps);
        let identity = AppIdentity::new(AppId::steam(3241660), "R.E.P.O.");

        assert_eq!(steam_appmanifest_path(&identity, &roots), Some(manifest));
        std::fs::remove_dir_all(root).unwrap();
    }
}
