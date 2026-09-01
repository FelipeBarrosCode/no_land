use std::collections::{BTreeMap, BTreeSet};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use chrono::Utc;
use noland_cas::{chunk_file_streaming, LocalCas};
use noland_classifier::Classifier;
use noland_crypto::MasterKey;
use noland_pack::{pack_chunk_files, pack_chunk_files_with_limits, PackIndexEntry};
use noland_rclone_adapter::{EphemeralRcloneSession, ProviderRootIdentity, TransferTuning};
use noland_snapshot::{create_view, discard};
use noland_state_core::*;
use noland_storage::{
    commit_bundle_with_index_for_operation, read_committed_manifest, read_pack_index,
    update_catalog_with_bundle, write_guarded_ephemeral_session, LocalStorage, RcloneStorage,
    SharedStorageProvider,
};
use uuid::Uuid;

use crate::reconcile::reconcile_app;
use crate::StateAgent;

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u64::MAX as u128) as u64
}

struct SnapshotCleanup(PathBuf);

impl Drop for SnapshotCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
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
    agent.db.set_operation_metrics(op.operation_id, metrics)?;
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
    performance: BackupPerformanceMode,
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
    let dirty_state = agent
        .db
        .list_dirty_apps()?
        .into_iter()
        .find(|dirty| dirty.app_id == *app_id);
    let pending_mutations = agent.db.pending_app_mutations(app_id, 10_000)?;
    metrics.num_dirty_paths = dirty_state
        .as_ref()
        .map(|dirty| dirty.dirty_paths.len() as u64)
        .unwrap_or(0);
    metrics.num_dirty_roots = agent.db.list_dirty_roots(Some(app_id))?.len() as u64;

    let parent = load_parent(provider, master, agent, app_id).await;
    if parent.is_some()
        && dirty_state.is_none()
        && pending_mutations.is_empty()
        && mode == BackupMode::PersonalState
    {
        let (parent_manifest, _) = parent.expect("checked parent");
        metrics.num_candidate_paths = 0;
        metrics.num_files_skipped_fast_identity = parent_manifest.files.len() as u64;
        metrics.total_duration_ms =
            elapsed_ms(total_started).saturating_add(metrics.discovery_duration_ms);
        mark_noop_completed(agent, &mut op, &mut metrics, &parent_manifest)?;
        return Ok(parent_manifest);
    }

    let requires_reconciliation = dirty_state
        .as_ref()
        .is_some_and(|dirty| dirty.requires_reconciliation)
        || agent
            .db
            .list_dirty_roots(Some(app_id))?
            .iter()
            .any(|root| root.requires_reconciliation);
    if requires_reconciliation {
        persist_operation(agent, &mut op, BackupState::Reconciling, &metrics)?;
        let started = Instant::now();
        let reconciled = reconcile_app(agent, app_id)?;
        metrics.reconciliation_duration_ms = elapsed_ms(started);
        metrics.num_files_scanned = reconciled as u64;
    }

    let planning_started = Instant::now();
    let classifier = Classifier::new(&agent.db, &agent.config.image_id);
    let roots = agent.roots.lock().clone();
    let mut candidates = BTreeMap::<String, (PathRecord, PathAssociation)>::new();
    let full_scope = parent.is_none()
        || mode == BackupMode::CompleteApplication
        || performance == BackupPerformanceMode::Full
        || requires_reconciliation;

    if full_scope {
        classifier.reclassify_app(app_id)?;
        let rows = if mode == BackupMode::CompleteApplication {
            agent.db.associations_for_app(app_id)?
        } else {
            agent.db.likely_backup_associations(app_id)?
        };
        for (record, association) in rows {
            candidates.insert(record.canonical_path.clone(), (record, association));
        }
    } else {
        for mutation in &pending_mutations {
            if let Some(record) = agent.db.get_path_by_canonical(&mutation.path)? {
                if let Some(association) = agent
                    .db
                    .associations_for_path(record.path_id)?
                    .into_iter()
                    .find(|association| association.app_id == *app_id)
                {
                    candidates.insert(record.canonical_path.clone(), (record, association));
                }
            }
        }
        if let Some(dirty) = &dirty_state {
            for path_id in &dirty.dirty_paths {
                if let Some(record) = agent.db.get_path_by_id(*path_id)? {
                    if let Some(association) = agent
                        .db
                        .associations_for_path(record.path_id)?
                        .into_iter()
                        .find(|association| association.app_id == *app_id)
                    {
                        candidates.insert(record.canonical_path.clone(), (record, association));
                    }
                }
            }
        }
        if candidates.is_empty() && dirty_state.is_some() {
            for (record, association) in agent.db.likely_backup_associations(app_id)? {
                candidates.insert(record.canonical_path.clone(), (record, association));
            }
        }
    }

    if let Some((record, association)) = track_steam_appmanifest(agent, &identity, &roots)? {
        candidates
            .entry(record.canonical_path.clone())
            .or_insert((record, association));
    }
    metrics.num_candidate_paths = candidates.len() as u64;

    let (mut manifest, mut pack_index) = match parent {
        Some((parent_manifest, parent_index)) => {
            let mut next = BundleManifest::new(
                ManifestApp::from(&identity),
                ManifestSource {
                    instance_id: agent.config.instance_id,
                    image_id: agent.config.image_id.clone(),
                    captured_at: Utc::now(),
                },
                mode,
            );
            next.parent_bundle_id = Some(parent_manifest.bundle_id);
            next.files = parent_manifest.files;
            (next, parent_index)
        }
        None => (
            BundleManifest::new(
                ManifestApp::from(&identity),
                ManifestSource {
                    instance_id: agent.config.instance_id,
                    image_id: agent.config.image_id.clone(),
                    captured_at: Utc::now(),
                },
                mode,
            ),
            Vec::new(),
        ),
    };

    let mut tombstones = BTreeSet::<(String, String)>::new();
    for mutation in &pending_mutations {
        if mutation.kind == AppMutationKind::Delete {
            if let Some(logical) = roots.classify(Path::new(&mutation.path)) {
                remove_manifest_file(&mut manifest, &logical);
                tombstones.insert((logical.logical_root.as_token(), logical.relative_path));
            }
        }
        if let Some(previous_path) = &mutation.previous_path {
            if let Some(logical) = roots.classify(Path::new(previous_path)) {
                remove_manifest_file(&mut manifest, &logical);
                tombstones.insert((logical.logical_root.as_token(), logical.relative_path));
            }
        }
    }

    let mut include_paths = Vec::new();
    let mut changed_files = BTreeMap::<String, (PathRecord, PathAssociation, LogicalPath)>::new();
    for (canonical, (record, association)) in candidates {
        let logical = roots.classify(Path::new(&canonical)).unwrap_or_else(|| {
            LogicalPath::new(
                LogicalRoot::Home,
                canonical.trim_start_matches('/').to_string(),
            )
        });
        let inherited = manifest_file(&manifest, &logical).cloned();
        remove_manifest_file(&mut manifest, &logical);
        if !Path::new(&canonical).is_file() {
            if manifest.parent_bundle_id.is_some() {
                tombstones.insert((logical.logical_root.as_token(), logical.relative_path));
            }
            continue;
        }
        match classifier.decide(&record, &association, mode)? {
            BackupDecision::Exclude
            | BackupDecision::MetadataOnly
            | BackupDecision::DeferAndReconcile => continue,
            _ => {}
        }

        let metadata = std::fs::metadata(&canonical)?;
        let mtime_ns = metadata_mtime_ns(&metadata).unwrap_or(0);
        #[cfg(unix)]
        let inode = Some(metadata.ino());
        #[cfg(not(unix))]
        let inode = None;
        let indexed = agent.db.get_file_state(
            app_id,
            &logical.logical_root.as_token(),
            &logical.relative_path,
        )?;
        if performance != BackupPerformanceMode::Full
            && indexed.as_ref().is_some_and(|state| {
                state.fast_identity_matches(metadata.len(), mtime_ns, inode, None)
            })
            && inherited.is_some()
        {
            manifest
                .files
                .push(inherited.expect("checked inherited file"));
            metrics.num_files_skipped_fast_identity =
                metrics.num_files_skipped_fast_identity.saturating_add(1);
            continue;
        }
        include_paths.push(PathBuf::from(&canonical));
        changed_files.insert(canonical, (record, association, logical));
    }

    for (logical_root, relative_path) in tombstones {
        manifest.tombstones.push(ManifestTombstone {
            logical_root,
            relative_path,
            reason: "observed deletion or rename".into(),
        });
    }
    metrics.planning_duration_ms = elapsed_ms(planning_started);
    let mut progress = OperationProgress::new("snapshotting", 0);
    progress.total_units = Some(include_paths.len() as u64);
    progress.detail_json = serde_json::json!({
        "performance_mode": performance.as_str(),
        "files_reused": metrics.num_files_skipped_fast_identity,
    });
    progress.unit = Some("files".into());
    agent.db.set_operation_progress(op_id, Some(&progress))?;

    if include_paths.is_empty() && manifest.tombstones.is_empty() {
        if let Some(parent_bundle_id) = manifest.parent_bundle_id {
            if let Ok(parent_manifest) =
                read_committed_manifest(provider, master, app_id, parent_bundle_id).await
            {
                metrics.total_duration_ms =
                    elapsed_ms(total_started).saturating_add(metrics.discovery_duration_ms);
                finish_backup_evidence(agent, app_id, &pending_mutations)?;
                mark_noop_completed(agent, &mut op, &mut metrics, &parent_manifest)?;
                return Ok(parent_manifest);
            }
        }
    }

    persist_operation(agent, &mut op, BackupState::Snapshotting, &metrics)?;
    let snapshot_started = Instant::now();
    let view = create_view(&agent.config.paths.snapshots, &include_paths, true)?;
    let _snapshot_cleanup = SnapshotCleanup(view.root.clone());
    manifest.consistency = view.consistency;
    metrics.snapshot_duration_ms = elapsed_ms(snapshot_started);

    persist_operation(agent, &mut op, BackupState::Hashing, &metrics)?;
    progress.phase = "hashing".into();
    progress.completed_units = 0;
    agent.db.set_operation_progress(op_id, Some(&progress))?;
    let hashing_started = Instant::now();
    let cas = LocalCas::new(agent.config.paths.cache.join("cas/chunks"))?;
    let storage_id = provider
        .storage_identity()
        .map(|identity| identity.cache_key());
    let mut inherited_hashes = pack_index
        .iter()
        .map(|entry| entry.chunk_hash.clone())
        .collect::<BTreeSet<_>>();
    let mut new_chunk_paths = BTreeMap::<String, PathBuf>::new();
    let mut small_file_chunk_hashes = BTreeSet::<String>::new();
    let mut trusted_states = Vec::<FileStateRecord>::new();

    for mapping in &view.mappings {
        let Some((record, association, logical)) =
            changed_files.get(&mapping.source.to_string_lossy().into_owned())
        else {
            continue;
        };
        let summary = chunk_file_streaming(&mapping.staged, |chunk, payload| {
            if inherited_hashes.contains(&chunk.hash) || new_chunk_paths.contains_key(&chunk.hash) {
                metrics.num_chunks_reused = metrics.num_chunks_reused.saturating_add(1);
                return Ok(());
            }
            if let Some(storage_id) = storage_id.as_deref() {
                if let Some(remote_entry) =
                    lookup_remote_pack_entry(agent, storage_id, &chunk.hash)?
                {
                    metrics.num_remote_index_hits = metrics.num_remote_index_hits.saturating_add(1);
                    metrics.num_chunks_reused = metrics.num_chunks_reused.saturating_add(1);
                    pack_index.push(remote_entry);
                    inherited_hashes.insert(chunk.hash.clone());
                    return Ok(());
                }
            }
            let stored = cas.put_verified(&chunk.hash, payload)?;
            remember_local_cas(
                agent,
                &chunk.hash,
                &stored.path,
                stored.bytes,
                stored.reused,
            )?;
            if stored.reused {
                metrics.num_local_cas_hits = metrics.num_local_cas_hits.saturating_add(1);
                metrics.bytes_reused_local =
                    metrics.bytes_reused_local.saturating_add(stored.bytes);
            } else {
                metrics.num_chunks_created = metrics.num_chunks_created.saturating_add(1);
            }
            new_chunk_paths.insert(chunk.hash.clone(), stored.path);
            Ok(())
        })?;
        metrics.num_files_rehashed = metrics.num_files_rehashed.saturating_add(1);
        metrics.bytes_scanned = metrics.bytes_scanned.saturating_add(summary.size);
        metrics.bytes_hashed = metrics.bytes_hashed.saturating_add(summary.size);
        metrics.bytes_chunked = metrics.bytes_chunked.saturating_add(summary.size);
        if noland_cas::is_small_file(summary.size) {
            metrics.num_small_files_packed = metrics.num_small_files_packed.saturating_add(1);
            small_file_chunk_hashes.extend(summary.chunks.iter().map(|chunk| chunk.hash.clone()));
        }
        let metadata = std::fs::metadata(&mapping.source)?;
        let current_mtime_ns = metadata_mtime_ns(&metadata);
        let mut current_record = record.clone();
        current_record.size = Some(summary.size.min(i64::MAX as u64) as i64);
        current_record.mtime_ns = current_mtime_ns;
        current_record.content_hash = Some(summary.file_hash.clone());
        #[cfg(unix)]
        {
            current_record.inode = Some(metadata.ino().min(i64::MAX as u64) as i64);
            current_record.mode = Some(metadata.mode() as i64);
            current_record.uid = Some(metadata.uid() as i64);
            current_record.gid = Some(metadata.gid() as i64);
        }
        let file = ManifestFile {
            logical_root: logical.logical_root.as_token(),
            relative_path: logical.relative_path.clone(),
            source_path_hint: Some(record.canonical_path.clone()),
            file_type: record.file_type.clone().unwrap_or_else(|| "file".into()),
            size: summary.size,
            file_hash: summary.file_hash.clone(),
            chunks: summary.chunks,
            mode: current_record
                .mode
                .and_then(|value| u32::try_from(value).ok()),
            mtime_ns: current_record.mtime_ns,
            uid: current_record
                .uid
                .and_then(|value| u32::try_from(value).ok()),
            gid: current_record
                .gid
                .and_then(|value| u32::try_from(value).ok()),
            symlink_target: None,
            persistence_class: association.persistence_class,
            semantic_role: association.semantic_role,
            association_confidence: association.confidence,
            shared_app_ids: agent
                .db
                .associations_for_path(record.path_id)?
                .into_iter()
                .filter(|other| {
                    other.app_id != *app_id && other.confidence >= OWNERSHIP_CANDIDATE_MIN
                })
                .map(|other| other.app_id)
                .collect(),
        };
        trusted_states.push(file_state_from_manifest(app_id, &file, &current_record));
        manifest.files.push(file);
        progress.completed_units = progress.completed_units.saturating_add(1);
        progress.detail_json = serde_json::json!({
            "bytes_hashed": metrics.bytes_hashed,
            "files_rehashed": metrics.num_files_rehashed,
        });
        progress.updated_at = Utc::now();
        agent.db.set_operation_progress(op_id, Some(&progress))?;
    }
    metrics.hashing_duration_ms = elapsed_ms(hashing_started);

    persist_operation(agent, &mut op, BackupState::Packing, &metrics)?;
    progress.phase = "packing".into();
    agent.db.set_operation_progress(op_id, Some(&progress))?;
    let packing_started = Instant::now();
    let pack_dir = agent
        .config
        .paths
        .packs
        .join(manifest.bundle_id.to_string());
    let mut small_chunks = Vec::new();
    let mut regular_chunks = Vec::new();
    for chunk in new_chunk_paths {
        if small_file_chunk_hashes.contains(&chunk.0) {
            small_chunks.push(chunk);
        } else {
            regular_chunks.push(chunk);
        }
    }
    // Small state files are deliberately grouped into compact packs so launch-critical
    // restore does not need to fetch a mostly unrelated 512 MiB pack.
    let mut packs = pack_chunk_files_with_limits(
        &pack_dir,
        master,
        small_chunks,
        |_| false,
        16 * 1024 * 1024,
        32 * 1024 * 1024,
    )?;
    packs.extend(pack_chunk_files(&pack_dir, master, regular_chunks, |_| {
        false
    })?);
    let mut incremental = 0u64;
    let mut pack_files = Vec::new();
    let mut new_pack_entries = Vec::<PackIndexEntry>::new();
    for pack in &packs {
        incremental = incremental.saturating_add(pack.bytes);
        metrics.bytes_packed = metrics.bytes_packed.saturating_add(pack.bytes);
        pack_files.push((pack.pack_id.clone(), pack.path.clone()));
        new_pack_entries.extend(pack.entries.iter().cloned());
    }
    pack_index.extend(new_pack_entries.iter().cloned());
    pack_index.sort_by(|left, right| left.chunk_hash.cmp(&right.chunk_hash));
    pack_index.dedup_by(|left, right| left.chunk_hash == right.chunk_hash);
    manifest.files.sort_by(|left, right| {
        (&left.logical_root, &left.relative_path).cmp(&(&right.logical_root, &right.relative_path))
    });
    manifest.tombstones.sort_by(|left, right| {
        (&left.logical_root, &left.relative_path).cmp(&(&right.logical_root, &right.relative_path))
    });
    noland_restore::embed_restore_plan(&mut manifest, restore_mode_for_backup(mode));
    metrics.packing_duration_ms = elapsed_ms(packing_started);

    persist_operation(agent, &mut op, BackupState::Uploading, &metrics)?;
    progress.phase = "uploading".into();
    progress.completed_units = 0;
    progress.total_units = Some(incremental);
    progress.unit = Some("bytes".into());
    progress.message = Some(format!("Uploading {incremental} bytes of changed state"));
    progress.detail_json = serde_json::json!({ "bytes_to_upload": incremental });
    progress.updated_at = Utc::now();
    agent.db.set_operation_progress(op_id, Some(&progress))?;
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
    commit_bundle_with_index_for_operation(
        provider,
        master,
        &manifest,
        &pack_files,
        Some(&index_json),
        Some(&agent.db),
        Some(op_id),
    )
    .await?;
    metrics.upload_duration_ms = elapsed_ms(upload_started);
    metrics.num_manifest_writes = 1;

    persist_operation(agent, &mut op, BackupState::Committing, &metrics)?;
    let commit_started = Instant::now();
    update_catalog_with_bundle(provider, master, &manifest, incremental).await?;
    cache_parent(agent, &manifest, &pack_index)?;
    metrics.commit_duration_ms = elapsed_ms(commit_started);
    let storage_metrics = provider.operation_metrics().saturating_sub(storage_before);
    apply_storage_metrics(&mut metrics, storage_metrics);

    for entry in &new_pack_entries {
        agent.db.remember_chunk(
            &entry.chunk_hash,
            Some(&entry.pack_id),
            entry.plaintext_len as u64,
        )?;
    }
    remember_remote_pack_index(agent, provider.storage_identity(), &pack_index)?;
    for state in &trusted_states {
        agent.db.upsert_file_state(state)?;
    }
    finish_backup_evidence(agent, app_id, &pending_mutations)?;

    persist_operation(agent, &mut op, BackupState::Checkpointing, &metrics)?;
    let checkpoint_started = Instant::now();
    let checkpoint_error = crate::checkpoint::write_local_checkpoint(agent)
        .err()
        .map(|error| error.to_string());
    if let Some(error) = checkpoint_error.as_ref() {
        tracing::error!(
            operation_id = %op_id,
            %error,
            "local checkpoint after backup commit failed"
        );
    }
    metrics.checkpoint_duration_ms = elapsed_ms(checkpoint_started);
    discard(&view)?;

    metrics.total_duration_ms =
        elapsed_ms(total_started).saturating_add(metrics.discovery_duration_ms);
    let detail = op
        .detail_json
        .as_object_mut()
        .expect("operation detail is always an object");
    detail.insert("bundle_id".into(), serde_json::json!(manifest.bundle_id));
    detail.insert("commit_id".into(), serde_json::json!(manifest.commit_id));
    if let Some(error) = checkpoint_error {
        detail.insert("checkpoint_error".into(), serde_json::json!(error));
    }
    agent.db.set_operation_progress(op_id, None)?;
    persist_operation(agent, &mut op, BackupState::Completed, &metrics)?;
    Ok(manifest)
}

async fn load_parent(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    agent: &StateAgent,
    app_id: &AppId,
) -> Option<(BundleManifest, Vec<PackIndexEntry>)> {
    let (_, bundle_id, _) = agent.db.latest_commit(app_id).ok().flatten()?;
    if let Some(cached) = read_cached_parent(agent, app_id, bundle_id) {
        let _ = remember_remote_pack_index(agent, provider.storage_identity(), &cached.1);
        return Some(cached);
    }
    let manifest = read_committed_manifest(provider, master, app_id, bundle_id)
        .await
        .ok()?;
    let index = read_pack_index(provider, master, app_id, bundle_id)
        .await
        .ok()?;
    cache_parent(agent, &manifest, &index).ok()?;
    let _ = remember_remote_pack_index(agent, provider.storage_identity(), &index);
    Some((manifest, index))
}

fn parent_cache_dir(agent: &StateAgent, app_id: &AppId, bundle_id: Uuid) -> PathBuf {
    let app_key = blake3::hash(app_id.as_str().as_bytes())
        .to_hex()
        .to_string();
    agent
        .config
        .paths
        .cache
        .join("manifests")
        .join(app_key)
        .join(bundle_id.to_string())
}

fn read_cached_parent(
    agent: &StateAgent,
    app_id: &AppId,
    bundle_id: Uuid,
) -> Option<(BundleManifest, Vec<PackIndexEntry>)> {
    let dir = parent_cache_dir(agent, app_id, bundle_id);
    let manifest: BundleManifest =
        serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).ok()?).ok()?;
    if manifest.bundle_id != bundle_id || manifest.app.app_id != *app_id {
        return None;
    }
    let index = serde_json::from_slice(&std::fs::read(dir.join("index.json")).ok()?).ok()?;
    Some((manifest, index))
}

pub(crate) fn remember_remote_pack_index(
    agent: &StateAgent,
    identity: Option<ProviderRootIdentity>,
    index: &[PackIndexEntry],
) -> Result<()> {
    let Some(identity) = identity else {
        return Ok(());
    };
    let storage_id = identity.cache_key();
    let now = Utc::now();
    for entry in index {
        agent.db.upsert_remote_content_entry(&RemoteContentEntry {
            storage_id: storage_id.clone(),
            object_kind: ContentObjectKind::Chunk,
            content_hash: entry.chunk_hash.clone(),
            remote_path: noland_state_core::pack_key(&entry.pack_id),
            size: Some(u64::from(entry.plaintext_len)),
            etag: Some(serde_json::to_string(entry)?),
            state: RemoteContentState::Present,
            observed_at: now,
            expires_at: None,
        })?;
    }
    Ok(())
}

fn lookup_remote_pack_entry(
    agent: &StateAgent,
    storage_id: &str,
    chunk_hash: &str,
) -> Result<Option<PackIndexEntry>> {
    let Some(entry) =
        agent
            .db
            .get_remote_content_by_hash(storage_id, ContentObjectKind::Chunk, chunk_hash)?
    else {
        return Ok(None);
    };
    if entry.state != RemoteContentState::Present || !entry.is_fresh_at(Utc::now()) {
        return Ok(None);
    }
    let Some(payload) = entry.etag else {
        return Ok(None);
    };
    Ok(serde_json::from_str(&payload).ok())
}

fn remember_local_cas(
    agent: &StateAgent,
    content_hash: &str,
    path: &Path,
    size: u64,
    reused: bool,
) -> Result<()> {
    let now = Utc::now();
    if reused {
        let _ = agent
            .db
            .touch_local_cas_entry(ContentObjectKind::Chunk, content_hash, now)?;
        if agent
            .db
            .get_local_cas_entry(ContentObjectKind::Chunk, content_hash)?
            .is_some()
        {
            return Ok(());
        }
    }
    agent.db.upsert_local_cas_entry(&LocalCasEntry {
        object_kind: ContentObjectKind::Chunk,
        content_hash: content_hash.to_string(),
        local_path: path.to_string_lossy().into_owned(),
        size,
        created_at: now,
        verified_at: Some(now),
        last_accessed_at: now,
    })
}

fn cache_parent(
    agent: &StateAgent,
    manifest: &BundleManifest,
    index: &[PackIndexEntry],
) -> Result<()> {
    let dir = parent_cache_dir(agent, &manifest.app.app_id, manifest.bundle_id);
    std::fs::create_dir_all(&dir)?;
    let manifest_temp = dir.join("manifest.json.tmp");
    let index_temp = dir.join("index.json.tmp");
    std::fs::write(&manifest_temp, serde_json::to_vec(manifest)?)?;
    std::fs::write(&index_temp, serde_json::to_vec(index)?)?;
    std::fs::rename(manifest_temp, dir.join("manifest.json"))?;
    std::fs::rename(index_temp, dir.join("index.json"))?;
    Ok(())
}

fn manifest_file<'a>(
    manifest: &'a BundleManifest,
    logical: &LogicalPath,
) -> Option<&'a ManifestFile> {
    manifest.files.iter().find(|file| {
        file.logical_root == logical.logical_root.as_token()
            && file.relative_path == logical.relative_path
    })
}

fn remove_manifest_file(manifest: &mut BundleManifest, logical: &LogicalPath) {
    manifest.files.retain(|file| {
        file.logical_root != logical.logical_root.as_token()
            || file.relative_path != logical.relative_path
    });
}

fn file_state_from_manifest(
    app_id: &AppId,
    file: &ManifestFile,
    record: &PathRecord,
) -> FileStateRecord {
    FileStateRecord {
        app_id: app_id.clone(),
        logical_root: file.logical_root.clone(),
        relative_path: file.relative_path.clone(),
        canonical_path: file.source_path_hint.clone(),
        file_type: match file.file_type.as_str() {
            "directory" => FileType::Directory,
            "symlink" => FileType::Symlink,
            "other" => FileType::Other,
            _ => FileType::File,
        },
        size: file.size,
        mtime_ns: file.mtime_ns.unwrap_or(0),
        inode: record.inode.and_then(|value| u64::try_from(value).ok()),
        mount_id: record.mount_id.and_then(|value| u64::try_from(value).ok()),
        mode: file.mode,
        content_hash: Some(file.file_hash.clone()),
        trust: FileStateTrust::Trusted,
        last_seen_at: Utc::now(),
        last_hashed_at: Some(Utc::now()),
    }
}

fn finish_backup_evidence(
    agent: &StateAgent,
    app_id: &AppId,
    mutations: &[AppMutationRecord],
) -> Result<()> {
    let mutation_ids = mutations
        .iter()
        .map(|mutation| mutation.mutation_id)
        .collect::<Vec<_>>();
    agent
        .db
        .mark_app_mutations_processed(&mutation_ids, Utc::now())?;
    if !agent.db.pending_app_mutations(app_id, 1)?.is_empty() {
        return Ok(());
    }
    agent.db.clear_dirty_roots(app_id)?;
    agent.db.clear_dirty(app_id)
}

fn mark_noop_completed(
    agent: &StateAgent,
    op: &mut OperationRecord,
    metrics: &mut OperationMetrics,
    manifest: &BundleManifest,
) -> Result<()> {
    let detail = op
        .detail_json
        .as_object_mut()
        .expect("operation detail is always an object");
    detail.insert("bundle_id".into(), serde_json::json!(manifest.bundle_id));
    detail.insert("commit_id".into(), serde_json::json!(manifest.commit_id));
    detail.insert("no_op".into(), serde_json::json!(true));
    agent.db.set_operation_progress(op.operation_id, None)?;
    persist_operation(agent, op, BackupState::Completed, metrics)
}

fn apply_storage_metrics(
    metrics: &mut OperationMetrics,
    storage_metrics: noland_storage::StorageOperationMetrics,
) {
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
}

fn metadata_mtime_ns(metadata: &std::fs::Metadata) -> Option<i64> {
    metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
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
    run_backup(
        agent,
        app_id,
        mode,
        BackupPerformanceMode::Balanced,
        &storage,
        master,
        None,
    )
    .await
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
    run_backup_with_session_performance(
        agent,
        app_id,
        mode,
        BackupPerformanceMode::Balanced,
        session,
        master,
        operation_id,
    )
    .await
}

pub async fn run_backup_with_session_performance(
    agent: &StateAgent,
    app_id: &AppId,
    mode: BackupMode,
    performance: BackupPerformanceMode,
    session: &EphemeralRcloneSession,
    master: &MasterKey,
    operation_id: Option<Uuid>,
) -> Result<BundleManifest> {
    let (config_path, _session_guard) =
        write_guarded_ephemeral_session(&agent.config.paths.run_root, session)?;
    let tuning = transfer_tuning(agent, performance);
    let storage = RcloneStorage::from_session(session, &config_path).with_transfer_tuning(tuning);
    run_backup(
        agent,
        app_id,
        mode,
        performance,
        &storage,
        master,
        operation_id,
    )
    .await
}

pub async fn run_backup_all_with_session(
    agent: &StateAgent,
    mode: BackupMode,
    session: &EphemeralRcloneSession,
    master: &MasterKey,
) -> Result<Vec<BundleManifest>> {
    run_backup_all_with_session_performance(
        agent,
        mode,
        BackupPerformanceMode::Balanced,
        session,
        master,
        None,
    )
    .await
}

pub async fn run_backup_all_with_session_performance(
    agent: &StateAgent,
    mode: BackupMode,
    performance: BackupPerformanceMode,
    session: &EphemeralRcloneSession,
    master: &MasterKey,
    operation_id: Option<Uuid>,
) -> Result<Vec<BundleManifest>> {
    let (config_path, _session_guard) =
        write_guarded_ephemeral_session(&agent.config.paths.run_root, session)?;
    let tuning = transfer_tuning(agent, performance);
    let storage = RcloneStorage::from_session(session, &config_path).with_transfer_tuning(tuning);
    run_backup_all(agent, mode, performance, &storage, master, operation_id).await
}

async fn run_backup_all(
    agent: &StateAgent,
    mode: BackupMode,
    performance: BackupPerformanceMode,
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    operation_id: Option<Uuid>,
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
    let mut out: Vec<BundleManifest> = Vec::new();
    provider.ensure_root().await?;
    let total = targets.len() as u64;
    for (index, app_id) in targets.iter().enumerate() {
        if let Some(operation_id) = operation_id {
            let mut progress = OperationProgress::new("backing_up_apps", index as u64);
            progress.total_units = Some(total);
            progress.unit = Some("apps".into());
            progress.message = Some(format!(
                "Backing up {} ({}/{})",
                app_id.as_str(),
                index + 1,
                total
            ));
            let completed_apps = out
                .iter()
                .map(|manifest| manifest.app.app_id.clone())
                .collect::<Vec<_>>();
            progress.detail_json = serde_json::json!({
                "app_id": app_id,
                "app_index": index,
                "app_count": total,
                "completed_apps": completed_apps,
            });
            agent
                .db
                .set_operation_progress(operation_id, Some(&progress))?;
        }
        match run_backup(agent, app_id, mode, performance, provider, master, None).await {
            Ok(manifest) => out.push(manifest),
            Err(StateError::NotFound(_)) => continue,
            Err(err) => return Err(err),
        }
    }
    Ok(out)
}

fn restore_mode_for_backup(mode: BackupMode) -> RestoreMode {
    match mode {
        BackupMode::PersonalState => RestoreMode::PersonalState,
        BackupMode::CompleteApplication => RestoreMode::CompleteApplication,
        BackupMode::Custom => RestoreMode::Custom,
    }
}

fn transfer_tuning(agent: &StateAgent, performance: BackupPerformanceMode) -> TransferTuning {
    if agent
        .db
        .open_session_app_ids()
        .is_ok_and(|sessions| !sessions.is_empty())
    {
        return TransferTuning::gameplay_safe();
    }
    match performance {
        BackupPerformanceMode::Fast | BackupPerformanceMode::Full => TransferTuning::throughput(),
        BackupPerformanceMode::Balanced => TransferTuning::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentConfig;
    use noland_storage::{read_pack_index, LocalStorage};

    fn test_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "noland-backup-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn tracked_save(
        agent: &StateAgent,
        app_id: &AppId,
        relative: &str,
        bytes: &[u8],
    ) -> (PathBuf, i64) {
        let path = agent.config.home.join(".config/test-game").join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        let canonical = path.to_string_lossy().into_owned();
        let path_id = agent.db.upsert_path(&canonical).unwrap();
        let now = Utc::now();
        agent
            .db
            .upsert_association(&PathAssociation {
                app_id: app_id.clone(),
                path_id,
                confidence: CONF_EXPLICIT,
                evidence: vec![Evidence::new(EvidenceKind::DirectCgroupWrite)],
                persistence_class: PersistenceClass::PersistentState,
                semantic_role: SemanticRole::UserState,
                first_seen_at: now,
                last_seen_at: now,
            })
            .unwrap();
        (path, path_id)
    }

    fn mark_mutated(
        agent: &StateAgent,
        app_id: &AppId,
        path: &Path,
        path_id: i64,
        kind: AppMutationKind,
    ) {
        agent
            .db
            .append_app_mutation(&AppMutationRecord::new(
                app_id.clone(),
                path.to_string_lossy(),
                kind,
            ))
            .unwrap();
        agent.db.mark_dirty(app_id, Some(path_id), false).unwrap();
        agent
            .db
            .mark_dirty_root(
                app_id,
                path.parent().unwrap().to_string_lossy().as_ref(),
                Some("$XDG_CONFIG_HOME"),
                false,
            )
            .unwrap();
    }

    #[tokio::test]
    async fn incremental_backup_rehashes_only_changed_file_and_preserves_parent_index() {
        let root = test_root("incremental");
        let cloud = root.join("cloud");
        let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
        let app = AppIdentity::new(AppId::desktop("test-game"), "Test Game");
        let app_id = app.app_id.clone();
        agent.db.upsert_app(&app).unwrap();
        let (first_path, first_id) =
            tracked_save(&agent, &app_id, "Mcd001.ps2", b"memory-card-one-v1");
        let _ = tracked_save(&agent, &app_id, "Mcd002.ps2", b"memory-card-two-v1");
        let master = MasterKey::generate();

        let first = run_backup_to_local(
            &agent,
            &app_id,
            BackupMode::PersonalState,
            cloud.clone(),
            &master,
        )
        .await
        .unwrap();
        assert_eq!(first.files.len(), 2);

        std::fs::write(&first_path, b"memory-card-one-v2").unwrap();
        mark_mutated(
            &agent,
            &app_id,
            &first_path,
            first_id,
            AppMutationKind::Modify,
        );
        let second = run_backup_to_local(
            &agent,
            &app_id,
            BackupMode::PersonalState,
            cloud.clone(),
            &master,
        )
        .await
        .unwrap();
        assert_eq!(second.parent_bundle_id, Some(first.bundle_id));
        assert_eq!(second.files.len(), 2);

        let operation = agent
            .db
            .recent_operations(10)
            .unwrap()
            .into_iter()
            .find(|operation| {
                operation.detail_json["bundle_id"] == serde_json::json!(second.bundle_id)
            })
            .unwrap();
        let metrics = operation_metrics(&operation);
        assert_eq!(metrics.num_candidate_paths, 1);
        assert_eq!(metrics.num_files_rehashed, 1);
        assert_eq!(metrics.num_files_skipped_fast_identity, 0);

        let storage = LocalStorage::new(cloud.clone());
        let index = read_pack_index(&storage, &master, &app_id, second.bundle_id)
            .await
            .unwrap();
        let indexed = index
            .iter()
            .map(|entry| entry.chunk_hash.as_str())
            .collect::<BTreeSet<_>>();
        for file in &second.files {
            for chunk in &file.chunks {
                assert!(indexed.contains(chunk.hash.as_str()));
            }
        }

        let no_op = run_backup_to_local(&agent, &app_id, BackupMode::PersonalState, cloud, &master)
            .await
            .unwrap();
        assert_eq!(no_op.bundle_id, second.bundle_id);
        let operation = agent
            .db
            .recent_operations(10)
            .unwrap()
            .into_iter()
            .find(|operation| operation.detail_json["no_op"] == true)
            .unwrap();
        let metrics = operation_metrics(&operation);
        assert_eq!(metrics.num_candidate_paths, 0);
        assert_eq!(metrics.num_files_rehashed, 0);
        assert_eq!(operation.detail_json["no_op"], true);
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn deletion_creates_tombstone_and_clears_evidence_only_after_commit() {
        let root = test_root("delete");
        let cloud = root.join("cloud");
        let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
        let app = AppIdentity::new(AppId::desktop("test-game"), "Test Game");
        let app_id = app.app_id.clone();
        agent.db.upsert_app(&app).unwrap();
        let (path, path_id) = tracked_save(&agent, &app_id, "Mcd001.ps2", b"memory-card");
        let master = MasterKey::generate();
        let first = run_backup_to_local(
            &agent,
            &app_id,
            BackupMode::PersonalState,
            cloud.clone(),
            &master,
        )
        .await
        .unwrap();

        std::fs::remove_file(&path).unwrap();
        mark_mutated(&agent, &app_id, &path, path_id, AppMutationKind::Delete);
        assert_eq!(
            agent.db.pending_app_mutations(&app_id, 10).unwrap().len(),
            1
        );
        let second =
            run_backup_to_local(&agent, &app_id, BackupMode::PersonalState, cloud, &master)
                .await
                .unwrap();
        assert_eq!(second.parent_bundle_id, Some(first.bundle_id));
        assert!(second.files.is_empty());
        assert_eq!(second.tombstones.len(), 1);
        assert_eq!(second.tombstones[0].relative_path, "test-game/Mcd001.ps2");
        assert!(agent
            .db
            .pending_app_mutations(&app_id, 10)
            .unwrap()
            .is_empty());
        assert!(agent.db.list_dirty_apps().unwrap().is_empty());
        std::fs::remove_dir_all(root).ok();
    }

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

    #[tokio::test]
    async fn remote_content_index_reuses_chunks_when_parent_index_is_absent() {
        let root = test_root("remote-reuse");
        let cloud = root.join("cloud");
        let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
        let app = AppIdentity::new(AppId::desktop("test-game"), "Test Game");
        let app_id = app.app_id.clone();
        agent.db.upsert_app(&app).unwrap();
        let (path, path_id) = tracked_save(&agent, &app_id, "Mcd001.ps2", b"memory-card-same");
        let master = MasterKey::generate();
        let first = run_backup_to_local(
            &agent,
            &app_id,
            BackupMode::PersonalState,
            cloud.clone(),
            &master,
        )
        .await
        .unwrap();
        let storage = LocalStorage::new(cloud.clone());
        let index = read_pack_index(&storage, &master, &app_id, first.bundle_id)
            .await
            .unwrap();
        remember_remote_pack_index(&agent, storage.storage_identity(), &index).unwrap();

        std::fs::remove_dir_all(parent_cache_dir(&agent, &app_id, first.bundle_id)).ok();
        agent
            .db
            .record_commit(
                Uuid::new_v4(),
                &app_id,
                Uuid::new_v4(),
                "missing-parent",
                None,
                CommitVisibility::Committed,
            )
            .unwrap();
        mark_mutated(&agent, &app_id, &path, path_id, AppMutationKind::Modify);
        let second =
            run_backup_to_local(&agent, &app_id, BackupMode::PersonalState, cloud, &master)
                .await
                .unwrap();
        let operation = agent
            .db
            .recent_operations(10)
            .unwrap()
            .into_iter()
            .find(|operation| {
                operation.detail_json["bundle_id"] == serde_json::json!(second.bundle_id)
            })
            .unwrap();
        let metrics = operation_metrics(&operation);
        assert!(
            metrics.num_remote_index_hits > 0,
            "expected remote pack-index hits, got {metrics:?}"
        );
        std::fs::remove_dir_all(root).ok();
    }
}
