use std::path::PathBuf;

use chrono::Utc;
use noland_cas::{blake3_file, chunk_file};
use noland_classifier::Classifier;
use noland_crypto::MasterKey;
use noland_pack::pack_chunks;
use noland_snapshot::{create_view, discard};
use noland_state_core::*;
use noland_rclone_adapter::EphemeralRcloneSession;
use noland_storage::{
    commit_bundle_with_index, update_catalog_with_bundle, shred_ephemeral_session,
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
            BackupDecision::Exclude | BackupDecision::MetadataOnly | BackupDecision::DeferAndReconcile => {
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
        let Some((record, assoc)) = files.iter().find(|(r, _)| {
            PathBuf::from(&r.canonical_path) == mapping.source
        }) else {
            continue;
        };
        let logical = roots
            .classify(&mapping.source)
            .unwrap_or_else(|| LogicalPath::new(LogicalRoot::Home, mapping.source.display().to_string()));
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
    let pack_dir = agent.config.paths.packs.join(manifest.bundle_id.to_string());
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
            agent
                .db
                .remember_chunk(&entry.chunk_hash, Some(&pack.pack_id), entry.plaintext_len as u64)?;
            noland_state_core::metrics::Metrics::inc(&agent.metrics.chunks_created_total);
        }
        noland_state_core::metrics::Metrics::add(&agent.metrics.pack_bytes_created_total, pack.bytes);
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
    let portable: std::collections::HashSet<String> = noland_discovery::filter_backup_candidates(
        agent.db.list_apps()?,
    )
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
