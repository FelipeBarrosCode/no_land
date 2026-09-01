use std::path::PathBuf;
use std::time::Instant;

use chrono::Utc;
use noland_cas::{blake3_file, chunk_file};
use noland_classifier::Classifier;
use noland_crypto::MasterKey;
use noland_pack::pack_chunks;
use noland_rclone_adapter::EphemeralRcloneSession;
use noland_snapshot::{create_view, discard};
use noland_state_core::*;
use noland_storage::{
    commit_bundle_with_index, update_catalog_with_bundle, write_guarded_ephemeral_session,
    LocalStorage, RcloneStorage, SharedStorageProvider,
};
use uuid::Uuid;

use crate::reconcile::reconcile_app;
use crate::StateAgent;

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u64::MAX as u128) as u64
}

fn operation_metrics(op: &OperationRecord) -> OperationMetrics {
    op.detail_json
        .get("metrics")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

fn persist_operation(
    agent: &StateAgent,
    op: &mut OperationRecord,
    state: BackupState,
    metrics: &OperationMetrics,
) -> Result<()> {
    op.state = state.as_str().into();
    op.updated_at = Utc::now();
    let detail = op
        .detail_json
        .as_object_mut()
        .expect("operation detail is always an object");
    detail.insert("metrics".into(), serde_json::to_value(metrics)?);
    agent.db.upsert_operation(op)?;
    tracing::info!(
        operation_id = %op.operation_id,
        app_id = op.app_id.as_ref().map(AppId::as_str),
        state = state.as_str(),
        metrics = %serde_json::to_string(metrics).unwrap_or_default(),
        "backup operation advanced"
    );
    Ok(())
}

pub async fn run_backup(
    agent: &StateAgent,
    app_id: &AppId,
    mode: BackupMode,
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    operation_id: Option<Uuid>,
) -> Result<BundleManifest> {
    let total_started = Instant::now();
    let op_id = operation_id.unwrap_or_else(Uuid::new_v4);
    let mut op = agent
        .db
        .get_operation(op_id)?
        .unwrap_or_else(|| OperationRecord {
            operation_id: op_id,
            kind: "backup".into(),
            app_id: Some(app_id.clone()),
            state: BackupState::Queued.as_str().into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            last_error: None,
            detail_json: serde_json::json!({}),
        });
    if !op.detail_json.is_object() {
        op.detail_json = serde_json::json!({});
    }
    let mut metrics = operation_metrics(&op);
    persist_operation(agent, &mut op, BackupState::Discovering, &metrics)?;

    let identity = agent
        .db
        .get_app(app_id)?
        .ok_or_else(|| StateError::NotFound(app_id.to_string()))?;

    let requires_reconciliation = agent
        .db
        .list_dirty_apps()?
        .into_iter()
        .any(|dirty| dirty.app_id == *app_id && dirty.requires_reconciliation);
    if requires_reconciliation {
        persist_operation(agent, &mut op, BackupState::Reconciling, &metrics)?;
        let started = Instant::now();
        let reconciled = reconcile_app(agent, app_id)?;
        metrics.reconciliation_duration_ms = elapsed_ms(started);
        metrics.num_files_scanned = metrics.num_files_scanned.saturating_add(reconciled as u64);
    }

    let planning_started = Instant::now();
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

    metrics.num_candidate_paths = associations.len() as u64;
    if let Some((record, association)) = track_steam_appmanifest(agent, &identity, &roots)? {
        let path = PathBuf::from(&record.canonical_path);
        if !include_paths.iter().any(|included| included == &path) {
            include_paths.push(path);
            files.push((record, association));
        }
    }

    metrics.planning_duration_ms = elapsed_ms(planning_started);
    persist_operation(agent, &mut op, BackupState::Snapshotting, &metrics)?;
    let snapshot_started = Instant::now();
    let view = create_view(&agent.config.paths.snapshots, &include_paths, true)?;
    metrics.snapshot_duration_ms = elapsed_ms(snapshot_started);

    persist_operation(agent, &mut op, BackupState::Hashing, &metrics)?;
    let hashing_started = Instant::now();

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
        metrics.num_files_rehashed = metrics.num_files_rehashed.saturating_add(1);
        metrics.bytes_scanned = metrics.bytes_scanned.saturating_add(chunks.size);
        metrics.bytes_hashed = metrics.bytes_hashed.saturating_add(chunks.size);
        metrics.bytes_chunked = metrics.bytes_chunked.saturating_add(chunks.size);
        let file_hash = if chunks.file_hash.is_empty() {
            blake3_file(&mapping.staged)?
        } else {
            chunks.file_hash.clone()
        };
        for (meta, payload) in chunks.chunks.iter().zip(chunks.payloads.iter()) {
            if !agent.db.known_chunk(&meta.hash)? {
                chunk_payloads.push((meta.hash.clone(), payload.clone()));
                metrics.num_chunks_created = metrics.num_chunks_created.saturating_add(1);
            } else {
                metrics.num_chunks_reused = metrics.num_chunks_reused.saturating_add(1);
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

    metrics.hashing_duration_ms = elapsed_ms(hashing_started);
    persist_operation(agent, &mut op, BackupState::Packing, &metrics)?;
    let packing_started = Instant::now();
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
        metrics.bytes_packed = metrics.bytes_packed.saturating_add(pack.bytes);
        noland_state_core::metrics::Metrics::add(
            &agent.metrics.pack_bytes_created_total,
            pack.bytes,
        );
    }
    metrics.packing_duration_ms = elapsed_ms(packing_started);

    persist_operation(agent, &mut op, BackupState::Uploading, &metrics)?;
    let storage_before = provider.operation_metrics();
    let upload_started = Instant::now();
    agent.db.record_commit(
        manifest.commit_id,
        app_id,
        manifest.bundle_id,
        "pending",
        None,
        CommitVisibility::Uploading,
    )?;

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
    metrics.upload_duration_ms = elapsed_ms(upload_started);
    metrics.num_manifest_writes = 1;

    persist_operation(agent, &mut op, BackupState::Committing, &metrics)?;
    let commit_started = Instant::now();
    update_catalog_with_bundle(provider, master, &manifest, incremental).await?;
    metrics.commit_duration_ms = elapsed_ms(commit_started);
    let storage_metrics = provider.operation_metrics().saturating_sub(storage_before);
    metrics.bytes_uploaded = metrics
        .bytes_uploaded
        .saturating_add(storage_metrics.bytes_uploaded);
    metrics.bytes_downloaded = metrics
        .bytes_downloaded
        .saturating_add(storage_metrics.bytes_downloaded);
    metrics.num_rclone_invocations = metrics
        .num_rclone_invocations
        .saturating_add(storage_metrics.rclone_invocations);
    metrics.num_remote_stat_calls = metrics
        .num_remote_stat_calls
        .saturating_add(storage_metrics.remote_stat_calls);
    metrics.num_remote_list_calls = metrics
        .num_remote_list_calls
        .saturating_add(storage_metrics.remote_list_calls);
    metrics.num_remote_mkdir_calls = metrics
        .num_remote_mkdir_calls
        .saturating_add(storage_metrics.remote_mkdir_calls);
    metrics.num_remote_upload_calls = metrics
        .num_remote_upload_calls
        .saturating_add(storage_metrics.remote_upload_calls);
    metrics.num_remote_download_calls = metrics
        .num_remote_download_calls
        .saturating_add(storage_metrics.remote_download_calls);

    persist_operation(agent, &mut op, BackupState::Checkpointing, &metrics)?;
    let checkpoint_started = Instant::now();
    let _ = crate::checkpoint::write_local_checkpoint(agent);
    metrics.checkpoint_duration_ms = elapsed_ms(checkpoint_started);

    agent.db.clear_dirty(app_id)?;
    discard(&view)?;
    metrics.total_duration_ms =
        elapsed_ms(total_started).saturating_add(metrics.discovery_duration_ms);
    let detail = op
        .detail_json
        .as_object_mut()
        .expect("operation detail is always an object");
    detail.insert("bundle_id".into(), serde_json::json!(manifest.bundle_id));
    detail.insert("commit_id".into(), serde_json::json!(manifest.commit_id));
    persist_operation(agent, &mut op, BackupState::Completed, &metrics)?;
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
    run_backup(agent, app_id, mode, &storage, master, None).await
}

/// Backup using an ephemeral rclone session minted by the desktop adapter.
pub async fn run_backup_with_session(
    agent: &StateAgent,
    app_id: &AppId,
    mode: BackupMode,
    session: &EphemeralRcloneSession,
    master: &MasterKey,
    operation_id: Option<Uuid>,
) -> Result<BundleManifest> {
    let (config_path, _session_guard) =
        write_guarded_ephemeral_session(&agent.config.paths.run_root, session)?;
    let storage = RcloneStorage::from_session(session, &config_path);
    run_backup(agent, app_id, mode, &storage, master, operation_id).await
}

pub async fn run_backup_all_with_session(
    agent: &StateAgent,
    mode: BackupMode,
    session: &EphemeralRcloneSession,
    master: &MasterKey,
) -> Result<Vec<BundleManifest>> {
    let (config_path, _session_guard) =
        write_guarded_ephemeral_session(&agent.config.paths.run_root, session)?;
    let storage = RcloneStorage::from_session(session, &config_path);
    run_backup_all(agent, mode, &storage, master).await
}

async fn run_backup_all(
    agent: &StateAgent,
    mode: BackupMode,
    provider: &dyn SharedStorageProvider,
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
    provider.ensure_root().await?;
    for app_id in targets {
        match run_backup(agent, &app_id, mode, provider, master, None).await {
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
