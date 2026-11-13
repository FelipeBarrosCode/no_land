use std::{
    path::{Path, PathBuf},
    time::Instant,
};

use noland_crypto::MasterKey;
use noland_rclone_adapter::{
    EphemeralRcloneSession, ProviderKind, TransferTuning, TransferWorkload,
};
use noland_restore::{
    prepare_restore, restore_pipelined, DownloadJournal, PipelineOptions, RestorePrerequisite,
    RestoreTarget,
};
use noland_state_core::*;
use noland_storage::{
    read_pack_index_for_operation, write_guarded_ephemeral_session, RcloneStorage,
    SharedStorageProvider,
};

use crate::StateAgent;

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u64::MAX as u128) as u64
}

fn persist_restore_progress(
    agent: &StateAgent,
    operation_id: uuid::Uuid,
    progress: &mut OperationProgress,
    phase: &str,
    completed_units: u64,
    message: &str,
) -> Result<()> {
    progress.phase = phase.into();
    progress.completed_units = completed_units;
    progress.message = Some(message.into());
    progress.updated_at = chrono::Utc::now();
    agent
        .db
        .set_operation_progress(operation_id, Some(progress))
}

fn persist_restore_failure(
    agent: &StateAgent,
    operation_id: uuid::Uuid,
    metrics: &OperationMetrics,
    error: &str,
) -> Result<()> {
    let Some(mut op) = agent.db.get_operation(operation_id)? else {
        return Ok(());
    };
    op.state = RestoreState::Failed.as_str().into();
    op.updated_at = chrono::Utc::now();
    op.last_error = Some(error.to_string());
    if !op.detail_json.is_object() {
        op.detail_json = serde_json::json!({});
    }
    op.detail_json
        .as_object_mut()
        .expect("operation detail is an object")
        .insert("metrics".into(), serde_json::to_value(metrics)?);
    agent.db.upsert_operation(&op)
}

fn persist_restore_operation(
    agent: &StateAgent,
    operation_id: uuid::Uuid,
    state: RestoreState,
    metrics: &OperationMetrics,
) -> Result<()> {
    let Some(mut op) = agent.db.get_operation(operation_id)? else {
        return Ok(());
    };
    op.state = state.as_str().into();
    op.updated_at = chrono::Utc::now();
    if !op.detail_json.is_object() {
        op.detail_json = serde_json::json!({});
    }
    op.detail_json
        .as_object_mut()
        .expect("operation detail is an object")
        .insert("metrics".into(), serde_json::to_value(metrics)?);
    agent.db.upsert_operation(&op)?;
    Ok(())
}

pub async fn run_restore_with_session(
    agent: &StateAgent,
    app_id: &AppId,
    bundle_id: uuid::Uuid,
    mode: RestoreMode,
    session: &EphemeralRcloneSession,
    master: &MasterKey,
    operation_id: uuid::Uuid,
) -> Result<()> {
    let (config, _session_guard) =
        write_guarded_ephemeral_session(&agent.config.paths.run_root, session)?;
    let provider = RcloneStorage::try_from_session(session, &config)?;
    let guard = crate::repository::acquire(agent, &provider, master, operation_id).await?;
    let result = run_restore_inner(
        agent,
        app_id,
        bundle_id,
        mode,
        session,
        master,
        operation_id,
    )
    .await;
    if let Err(error) = guard.release().await {
        tracing::warn!(%error,"restore repository admission cleanup requires owner recovery");
    }
    result
}

async fn run_restore_inner(
    agent: &StateAgent,
    app_id: &AppId,
    bundle_id: uuid::Uuid,
    mode: RestoreMode,
    session: &EphemeralRcloneSession,
    master: &MasterKey,
    operation_id: uuid::Uuid,
) -> Result<()> {
    let total_started = Instant::now();
    let mut metrics = OperationMetrics::default();
    let mut progress = OperationProgress::new(RestoreState::FetchingManifest.as_str(), 0);
    progress.unit = Some("files".into());
    progress.detail_json = serde_json::json!({
        "bundle_id": bundle_id,
        "ready_to_launch_reached": false,
        "milestones": [],
    });
    persist_restore_operation(
        agent,
        operation_id,
        RestoreState::FetchingManifest,
        &metrics,
    )?;
    persist_restore_progress(
        agent,
        operation_id,
        &mut progress,
        RestoreState::FetchingManifest.as_str(),
        0,
        "Fetching and verifying the restore manifest",
    )?;
    let (config_path, _session_guard) =
        write_guarded_ephemeral_session(&agent.config.paths.run_root, session)?;
    let provider = ProviderKind::parse(&session.provider).unwrap_or(ProviderKind::Local);
    let workload = if agent.has_latency_sensitive_sessions()? {
        TransferWorkload::GameplaySafe
    } else {
        TransferWorkload::Throughput
    };
    let tuning = TransferTuning::automatic(provider, workload);
    tracing::info!(
        provider = provider.as_str(),
        ?workload,
        uploads = tuning.max_parallel_uploads,
        downloads = tuning.max_parallel_downloads,
        rclone_transfers = tuning.rclone_transfers,
        "resolved automatic shared-storage restore parallelism"
    );
    let pipeline_options = PipelineOptions {
        // Streaming uses one ordered connection per pack, not copyto multistream.
        max_parallel_packs: tuning.max_parallel_downloads.max(1),
        reconstruction_workers: if workload == TransferWorkload::GameplaySafe {
            1
        } else {
            PipelineOptions::default().reconstruction_workers
        },
        ..PipelineOptions::default()
    };
    let transfer_plan = serde_json::json!({
        "workload": format!("{workload:?}"),
        "logical_cpus": std::thread::available_parallelism().map_or(1, usize::from),
        "max_parallel_packs": pipeline_options.max_parallel_packs,
        "verification_workers": pipeline_options.reconstruction_workers,
        "buffered_chunks": pipeline_options.buffered_chunks,
        "max_inflight_pack_bytes": pipeline_options.max_inflight_pack_bytes,
    });
    let storage =
        RcloneStorage::try_from_session(session, &config_path)?.with_transfer_tuning(tuning);
    let storage_before = storage.operation_metrics();

    // Dynamic roots such as Steam libraries may appear after the agent starts.
    // Refresh them before resolving the portable logical roots in the manifest.
    let discovery_started = Instant::now();
    if let Err(error) = agent.discover() {
        return Err(error);
    }
    metrics.discovery_duration_ms = elapsed_ms(discovery_started);

    let manifest_started = Instant::now();
    let plan = prepare_restore(
        &storage,
        master,
        &agent.config.paths,
        app_id,
        bundle_id,
        mode,
    )
    .await;
    metrics.manifest_duration_ms = elapsed_ms(manifest_started);
    let result = match plan {
        Ok(plan) => {
            let plan = plan.into_shared();
            if plan.manifest.files.is_empty()
                && (mode == RestoreMode::CompleteApplication || plan.manifest.tombstones.is_empty())
            {
                return Err(StateError::Invalid(format!(
                    "bundle {bundle_id} contains no restorable files"
                )));
            }
            let priority_plan = plan.priority_plan();
            let total_files = u64::try_from(priority_plan.entries.len()).unwrap_or(u64::MAX);
            let ready_to_launch_files = u64::try_from(
                priority_plan
                    .entries_for(RestoreTarget::ReadyToLaunch)
                    .count(),
            )
            .unwrap_or(u64::MAX);
            progress.total_units = Some(total_files);
            progress.detail_json = serde_json::json!({
                "bundle_id": bundle_id,
                "restore_id": plan.restore_id,
                "ready_to_launch_files": ready_to_launch_files,
                "total_files": total_files,
                "ready_to_launch_reached": false,
                "milestones": [],
                "target": "READY_TO_LAUNCH",
                "transfer_plan": transfer_plan,
            });
            persist_restore_operation(
                agent,
                operation_id,
                RestoreState::CheckingPrerequisites,
                &metrics,
            )?;
            persist_restore_progress(
                agent,
                operation_id,
                &mut progress,
                RestoreState::CheckingPrerequisites.as_str(),
                0,
                "Planning launch-critical and remaining restore work",
            )?;

            let restore_result = async {
                let roots = prepare_restore_roots(agent, &plan.manifest)?;
                let local_plan = plan.clone().with_prerequisites(steam_restore_prerequisites(&plan.manifest, &roots, mode));
                let manifest_app = &plan.manifest.app;
                agent.db.upsert_app(&AppIdentity {
                    app_id: manifest_app.app_id.clone(),
                    display_name: manifest_app.display_name.clone(),
                    canonical_executable: manifest_app.canonical_executable.clone(),
                    desktop_entry_id: manifest_app.desktop_entry_id.clone(),
                    steam_app_id: manifest_app.steam_app_id,
                    launcher: manifest_app.launcher,
                    aliases: manifest_app.aliases.clone(),
                    identity_confidence: 1.0,
                    icon_path: manifest_app.icon_path.clone(),
                })?;

                let index = read_pack_index_for_operation(
                    &storage,
                    master,
                    app_id,
                    bundle_id,
                    Some(&agent.db),
                    Some(operation_id),
                )
                .await?;
                let index_cache_started = Instant::now();
                crate::backup::remember_remote_pack_index(
                    agent,
                    storage.storage_identity(),
                    master,
                    &index,
                )?;
                progress.detail_json["remote_index_cache_us"] = serde_json::json!(index_cache_started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64);
                // Restore retries reuse verified local pack/chunk caches directly. Their old
                // journal rows are not needed for resume and would pollute this attempt's totals.
                agent.db.delete_sync_journal_entries_for_kind_direction(
                    operation_id,
                    ContentObjectKind::Pack,
                    SyncDirection::Download,
                )?;
                let download_journal = Some(DownloadJournal {
                    db: &agent.db,
                    operation_id,
                });

                persist_restore_operation(agent, operation_id, RestoreState::Applying, &metrics)?;
                let report = restore_pipelined(
                    &storage,
                    master,
                    &local_plan,
                    &index,
                    &roots,
                    Some(&agent.db),
                    download_journal,
                    pipeline_options,
                    |sample| {
                        progress.total_units = Some(sample.total_entries);
                        progress.unit = Some("files".into());
                        progress.detail_json["ready_to_launch_reached"] =
                            serde_json::json!(sample.ready_to_launch);
                        progress.detail_json["pipeline"] = serde_json::json!({
                            "phase": sample.phase,
                            "checked_files": sample.checked_entries,
                            "rollback_files": sample.rollback_entries,
                            "preflight_duration_ms": sample.preflight_duration_ms,
                            "rollback_duration_ms": sample.rollback_duration_ms,
                            "version": 2,
                            "assembled_bytes": sample.assembled_bytes,
                            "verified_bytes": sample.verified_bytes,
                            "readback_bytes": sample.readback_bytes,
                            "published_bytes": sample.published_bytes,
                            "total_bytes": sample.total_bytes,
                            "reused_files": sample.reused_entries,
                            "elapsed_ms": sample.elapsed_ms,
                            "files_per_second": sample.completed_entries as f64 * 1000.0 / sample.elapsed_ms.max(1) as f64,
                            "assembled_bytes_per_second": sample.assembled_bytes as f64 * 1000.0 / sample.elapsed_ms.max(1) as f64,
                            "io": sample.io,
                        });
                        if sample.ready_to_launch {
                            progress.detail_json["milestones"] =
                                serde_json::json!(["READY_TO_LAUNCH"]);
                        }
                        persist_restore_progress(
                            agent,
                            operation_id,
                            &mut progress,
                            RestoreState::Applying.as_str(),
                            sample.completed_entries,
                            &format!("{} · {:.1} MiB assembled · {:.1} MiB verified · {:.1} MiB extra read-back", sample.phase,
                                sample.assembled_bytes as f64 / 1_048_576.0,
                                sample.verified_bytes as f64 / 1_048_576.0,
                                sample.readback_bytes as f64 / 1_048_576.0),
                        )
                    },
                )
                .await?;
                metrics.restore_apply_duration_ms = report.apply_duration_ms;
                metrics.download_duration_ms = report.pipeline_duration_ms;
                progress.detail_json["pipeline"]["io"] = serde_json::to_value(&report.io)?;
                progress.detail_json["pipeline"]["elapsed_ms"] = serde_json::json!(report.pipeline_duration_ms);
                progress.detail_json["total_packs"] = serde_json::json!(report.packs_downloaded);
                progress.detail_json["complete_published_files"] =
                    serde_json::json!(report.restore.published_entries);
                progress.detail_json["complete_reused_files"] =
                    serde_json::json!(report.restore.reused_entries);
                progress.detail_json["milestones"] =
                    serde_json::json!(["READY_TO_LAUNCH", "COMPLETE"]);
                // A personal-state selection from a complete bundle is not a
                // complete local parent. Only a successful, fully selected
                // publication may enable backup's unchanged-parent shortcut.
                let restored_all_files = priority_plan.entries.len() == plan.manifest.files.len()
                    && report.restore.published_entries + report.restore.reused_entries
                        == plan.manifest.files.len();
                let cache_started = Instant::now();
                let cache_result = if restored_all_files {
                    crate::backup::remember_restored_parent(
                        agent,
                        storage.storage_identity(),
                        master,
                        &plan.manifest,
                        &index,
                    )
                } else {
                    Ok(())
                };
                progress.detail_json["post_commit_cache_us"] = serde_json::json!(cache_started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64);
                if let Err(error) = cache_result {
                        // The filesystem and index already committed. Cache
                        // failure must not relabel that durable restore failed.
                        tracing::warn!(%error, %bundle_id, "restored backup parent cache unavailable");
                        progress.detail_json["incremental_parent_cache_error"] =
                            serde_json::json!(error.to_string());
                }
                progress.detail_json["restored_all_manifest_files"] = serde_json::json!(restored_all_files);
                Ok(())
            }
            .await;

            let terminal_pack_release = plan.release_staged_pack_links();
            // Streaming owns only this workspace's fallback scratch files. Do not
            // prune other operations' shared pack cache while releasing our links.
            let terminal_pack_cleanup = terminal_pack_release;
            match restore_result {
                Ok(()) => {
                    let released = terminal_pack_cleanup?;
                    progress.detail_json["terminal_pack_cache_cleanup"] = serde_json::json!({
                        "staged_links_released": released,
                    });
                    Ok(())
                }
                Err(error) => {
                    if let Err(cleanup_error) = terminal_pack_cleanup {
                        tracing::warn!(
                            restore_id = %plan.restore_id,
                            %cleanup_error,
                            "failed to clear restore pack cache after terminal failure"
                        );
                    }
                    Err(error)
                }
            }
        }
        Err(error) => Err(error),
    };
    let storage_metrics = storage.operation_metrics().saturating_sub(storage_before);
    metrics.bytes_downloaded = storage_metrics.bytes_downloaded;
    metrics.bytes_uploaded = storage_metrics.bytes_uploaded;
    metrics.num_rclone_invocations = storage_metrics.rclone_invocations;
    metrics.num_remote_stat_calls = storage_metrics.remote_stat_calls;
    metrics.num_remote_list_calls = storage_metrics.remote_list_calls;
    metrics.num_remote_mkdir_calls = storage_metrics.remote_mkdir_calls;
    metrics.num_remote_upload_calls = storage_metrics.remote_upload_calls;
    metrics.num_remote_download_calls = storage_metrics.remote_download_calls;
    metrics.total_duration_ms = elapsed_ms(total_started);
    match &result {
        Ok(()) => {
            let completed_units = progress.total_units.unwrap_or(progress.completed_units);
            persist_restore_progress(
                agent,
                operation_id,
                &mut progress,
                RestoreState::Completed.as_str(),
                completed_units,
                "Restore completed",
            )?;
            persist_restore_operation(agent, operation_id, RestoreState::Completed, &metrics)?;
        }
        Err(error) => {
            let completed_units = progress.completed_units;
            progress.detail_json["failed"] = serde_json::json!(true);
            if let Err(persist_error) = persist_restore_progress(
                agent,
                operation_id,
                &mut progress,
                RestoreState::Failed.as_str(),
                completed_units,
                &error.to_string(),
            ) {
                tracing::warn!(
                    %operation_id,
                    %persist_error,
                    "failed to persist restore failure progress"
                );
            }
            if let Err(persist_error) =
                persist_restore_failure(agent, operation_id, &metrics, &error.to_string())
            {
                tracing::warn!(
                    %operation_id,
                    %persist_error,
                    "failed to persist restore failure metrics"
                );
            }
        }
    }
    result
}

fn steam_restore_prerequisites(
    bundle: &BundleManifest,
    roots: &LogicalRootMap,
    mode: RestoreMode,
) -> Vec<RestorePrerequisite> {
    if mode != RestoreMode::CompleteApplication {
        return Vec::new();
    }
    let Some(steam_app_id) = bundle.app.steam_app_id.or_else(|| {
        bundle
            .app
            .app_id
            .as_str()
            .strip_prefix("steam:")
            .and_then(|value| value.parse::<u32>().ok())
    }) else {
        return Vec::new();
    };
    let Some((target_dir, install_dir)) = steam_install_location_from_manifest(bundle, roots)
    else {
        return Vec::new();
    };
    let manifest = format!(
        "\"AppState\"\n{{\n\t\"appid\"\t\t\"{steam_app_id}\"\n\t\"name\"\t\t\"{}\"\n\t\"StateFlags\"\t\t\"4\"\n\t\"installdir\"\t\t\"{}\"\n}}\n",
        escape_acf_value(&bundle.app.display_name),
        escape_acf_value(&install_dir),
    );
    vec![RestorePrerequisite {
        root: target_dir,
        relative_path: format!("appmanifest_{steam_app_id}.acf"),
        contents: manifest.into_bytes(),
    }]
}

fn prepare_restore_roots(agent: &StateAgent, bundle: &BundleManifest) -> Result<LogicalRootMap> {
    let mut roots = agent.roots.lock().clone();
    let Some(preferred_steamapps) = preferred_steamapps_dir(&roots) else {
        return Ok(roots);
    };

    for library_id in bundle.files.iter().filter_map(|file| {
        let LogicalRoot::SteamLibrary { id } = file.logical_root_parsed()? else {
            return None;
        };
        Some(id)
    }) {
        roots
            .steam_libraries
            .entry(library_id)
            .or_insert_with(|| preferred_steamapps.clone());
    }

    let steam_app_id = bundle.app.steam_app_id.or_else(|| {
        bundle
            .app
            .app_id
            .as_str()
            .strip_prefix("steam:")
            .and_then(|value| value.parse::<u32>().ok())
    });
    if let Some(steam_app_id) = steam_app_id {
        roots
            .proton_prefixes
            .entry(steam_app_id)
            .or_insert_with(|| {
                preferred_steamapps
                    .join("compatdata")
                    .join(steam_app_id.to_string())
                    .join("pfx")
            });
    }

    let mut live_roots = agent.roots.lock();
    for (id, path) in &roots.steam_libraries {
        live_roots
            .steam_libraries
            .entry(id.clone())
            .or_insert_with(|| path.clone());
    }
    for (id, path) in &roots.proton_prefixes {
        live_roots
            .proton_prefixes
            .entry(*id)
            .or_insert_with(|| path.clone());
    }
    drop(live_roots);

    Ok(roots)
}

fn preferred_steamapps_dir(roots: &LogicalRootMap) -> Option<PathBuf> {
    roots
        .steam_libraries
        .values()
        .next()
        .cloned()
        .or_else(|| roots.steam_root.as_ref().map(|root| root.join("steamapps")))
}

fn steam_install_location_from_manifest(
    bundle: &BundleManifest,
    roots: &LogicalRootMap,
) -> Option<(PathBuf, String)> {
    let logical_location = bundle.files.iter().find_map(|file| {
        let Some(LogicalRoot::SteamLibrary { id }) = file.logical_root_parsed() else {
            return None;
        };
        let install_dir = steam_install_dir_from_path(Path::new(&file.relative_path))?;
        Some((roots.steam_libraries.get(&id)?.clone(), install_dir))
    });
    if logical_location.is_some() {
        return logical_location;
    }

    let install_dir = bundle
        .files
        .iter()
        .find_map(|file| {
            file.source_path_hint
                .as_deref()
                .and_then(|path| steam_install_dir_from_path(Path::new(path)))
        })
        .or_else(|| {
            bundle
                .app
                .canonical_executable
                .as_deref()
                .and_then(steam_install_dir_from_path)
        })?;
    Some((preferred_steamapps_dir(roots)?, install_dir))
}

fn steam_install_dir_from_path(path: &Path) -> Option<String> {
    let components = path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>();
    components
        .windows(3)
        .find(|parts| {
            parts[0].eq_ignore_ascii_case("steamapps") && parts[1].eq_ignore_ascii_case("common")
        })
        .map(|parts| parts[2].to_string())
        .or_else(|| {
            components
                .first()
                .is_some_and(|component| component.eq_ignore_ascii_case("common"))
                .then(|| components.get(1).map(|component| (*component).to_string()))
                .flatten()
        })
        .filter(|install_dir| !install_dir.is_empty())
}

fn escape_acf_value(value: &str) -> String {
    value.replace('"', "'")
}

#[cfg(test)]
mod tests {
    use super::{
        preferred_steamapps_dir, prepare_restore_roots, steam_install_dir_from_path,
        steam_restore_prerequisites,
    };
    use crate::{AgentConfig, StateAgent};
    use chrono::Utc;
    use noland_state_core::{
        AppId, AppIdentity, BackupMode, BundleManifest, LogicalRootMap, ManifestApp,
        ManifestSource, RestoreMode,
    };
    use std::path::{Path, PathBuf};
    use uuid::Uuid;

    fn steam_bundle(display_name: &str) -> BundleManifest {
        BundleManifest::new(
            ManifestApp::from(&AppIdentity::new(AppId::steam(3_241_660), display_name)),
            ManifestSource {
                instance_id: Uuid::new_v4(),
                image_id: "test".into(),
                captured_at: Utc::now(),
            },
            BackupMode::CompleteApplication,
        )
    }

    #[test]
    fn steam_install_dir_comes_from_install_path_not_display_name() {
        assert_eq!(
            steam_install_dir_from_path(Path::new(
                "/mnt/library/steamapps/common/RepoInstall_3241660/bin/game.exe"
            )),
            Some("RepoInstall_3241660".into())
        );
        assert_eq!(
            steam_install_dir_from_path(Path::new("common/RepoInstall_3241660/game.bin")),
            Some("RepoInstall_3241660".into())
        );
    }

    #[tokio::test]
    async fn restored_external_steam_save_survives_complete_backup_and_library_remapping() {
        use noland_crypto::MasterKey;
        use noland_restore::{prepare_restore, restore_pipelined, PipelineOptions};
        use noland_state_core::*;
        use noland_storage::{
            commit_bundle_with_index, read_committed_manifest, read_pack_index, LocalStorage,
            SharedStorageProvider,
        };
        let root = std::fs::canonicalize(std::env::current_dir().unwrap())
            .unwrap()
            .join(format!("target/noland-restore-parent-{}", Uuid::new_v4()));
        let agent = StateAgent::boot(AgentConfig::isolated(root.join("target"))).unwrap();
        let storage = LocalStorage::new(root.join("cloud"));
        let master = MasterKey::generate();
        let mut source = steam_bundle("TestGame");
        source.environment["capture_scope_version"] = serde_json::json!(1);
        let app = source.app.app_id.clone();
        agent
            .db
            .upsert_app(&AppIdentity::new(app.clone(), "TestGame"))
            .unwrap();
        let library = agent.config.home.join("SteamLibrary/steamapps");
        agent.roots.lock().steam_libraries.clear();
        agent
            .roots
            .lock()
            .steam_libraries
            .insert("0".into(), library.clone());
        let chunks = [
            (
                "$STEAM_LIBRARY:1",
                "common/TestGame/game.bin",
                b"game".as_slice(),
            ),
            (
                "$XDG_CONFIG_HOME",
                "external-save/save.dat",
                b"save".as_slice(),
            ),
        ];
        for (logical_root, relative_path, bytes) in chunks {
            let hash = noland_cas::blake3_hex(bytes);
            source.files.push(ManifestFile {
                logical_root: logical_root.into(),
                relative_path: relative_path.into(),
                source_path_hint: Some(format!("/source-pc/{relative_path}")),
                file_type: "file".into(),
                size: bytes.len() as u64,
                file_hash: hash.clone(),
                chunks: vec![ChunkRef {
                    hash,
                    size: bytes.len() as u64,
                }],
                mode: Some(0o600),
                mtime_ns: Some(1),
                uid: None,
                gid: None,
                symlink_target: None,
                persistence_class: if logical_root.starts_with("$STEAM") {
                    PersistenceClass::ReconstructableApp
                } else {
                    PersistenceClass::PersistentState
                },
                semantic_role: if logical_root.starts_with("$STEAM") {
                    SemanticRole::AppContent
                } else {
                    SemanticRole::UserState
                },
                association_confidence: 1.0,
                shared_app_ids: Vec::new(),
            });
        }
        let packs = noland_pack::pack_chunks(
            &root.join("packs"),
            &master,
            chunks
                .into_iter()
                .map(|(_, _, bytes)| (noland_cas::blake3_hex(bytes), bytes.to_vec())),
            |_| false,
        )
        .unwrap();
        let index = packs
            .iter()
            .flat_map(|pack| pack.entries.clone())
            .collect::<Vec<_>>();
        let pack_files = packs
            .iter()
            .map(|pack| (pack.pack_id.clone(), pack.path.clone()))
            .collect::<Vec<_>>();
        commit_bundle_with_index(
            &storage,
            &master,
            &source,
            &pack_files,
            Some(&serde_json::to_vec(&index).unwrap()),
            None,
        )
        .await
        .unwrap();
        let destination = agent.config.home.join(".config/external-save/save.dat");
        let path_id = agent.db.upsert_path(destination.to_str().unwrap()).unwrap();
        agent
            .db
            .upsert_association(&PathAssociation {
                app_id: app.clone(),
                path_id,
                confidence: 0.5,
                evidence: vec![Evidence::new(EvidenceKind::NameHeuristic)
                    .with_detail("preserve existing evidence")],
                persistence_class: PersistenceClass::PersistentState,
                semantic_role: SemanticRole::UserState,
                first_seen_at: Utc::now(),
                last_seen_at: Utc::now(),
            })
            .unwrap();
        let roots = prepare_restore_roots(&agent, &source).unwrap();
        let plan = prepare_restore(
            &storage,
            &master,
            &agent.config.paths,
            &app,
            source.bundle_id,
            RestoreMode::CompleteApplication,
        )
        .await
        .unwrap()
        .into_shared();
        restore_pipelined(
            &storage,
            &master,
            &plan,
            &index,
            &roots,
            Some(&agent.db),
            None,
            PipelineOptions::default(),
            |_| Ok(()),
        )
        .await
        .unwrap();
        crate::backup::remember_remote_pack_index(
            &agent,
            storage.storage_identity(),
            &master,
            &index,
        )
        .unwrap();
        crate::backup::remember_restored_parent(
            &agent,
            storage.storage_identity(),
            &master,
            &source,
            &index,
        )
        .unwrap();
        let associations = agent.db.associations_for_app(&app).unwrap();
        let evidence = &associations
            .iter()
            .find(|(path, _)| Path::new(&path.canonical_path) == destination)
            .unwrap()
            .1
            .evidence;
        assert!(evidence
            .iter()
            .any(|item| item.kind == EvidenceKind::ExplicitUserBinding));
        assert!(evidence
            .iter()
            .any(|item| item.detail.as_deref() == Some("preserve existing evidence")));
        assert!(agent
            .db
            .get_file_state(&app, "$STEAM_LIBRARY:0", "common/TestGame/game.bin")
            .unwrap()
            .is_some());
        assert!(agent
            .db
            .get_file_state(&app, "$STEAM_LIBRARY:1", "common/TestGame/game.bin")
            .unwrap()
            .is_none());
        let unchanged_operation = Uuid::new_v4();
        let unchanged = crate::backup::run_backup(
            &agent,
            &app,
            BackupMode::CompleteApplication,
            BackupPerformanceMode::Balanced,
            &storage,
            &master,
            Some(unchanged_operation),
        )
        .await
        .unwrap();
        assert_eq!(unchanged.files.len(), 2);
        assert!(unchanged.tombstones.is_empty());
        let metrics = agent
            .db
            .get_operation_metrics(unchanged_operation)
            .unwrap()
            .unwrap();
        assert_eq!(metrics.num_files_rehashed, 0);
        assert_eq!(metrics.num_files_skipped_fast_identity, 2);
        assert_eq!(metrics.bytes_uploaded, 0);
        std::fs::write(&destination, b"changed save").unwrap();
        agent
            .db
            .append_app_mutation(&AppMutationRecord::new(
                app.clone(),
                destination.to_str().unwrap(),
                AppMutationKind::Modify,
            ))
            .unwrap();
        agent.db.mark_dirty(&app, Some(path_id), false).unwrap();
        let changed_operation = Uuid::new_v4();
        let next = crate::backup::run_backup(
            &agent,
            &app,
            BackupMode::CompleteApplication,
            BackupPerformanceMode::Balanced,
            &storage,
            &master,
            Some(changed_operation),
        )
        .await
        .unwrap();
        let metrics = agent
            .db
            .get_operation_metrics(changed_operation)
            .unwrap()
            .unwrap();
        assert_eq!(metrics.num_files_rehashed, 1);
        assert_eq!(metrics.num_files_skipped_fast_identity, 1);
        assert_eq!(next.files.len(), 2);
        assert!(
            next.tombstones.is_empty(),
            "scope selection must not synthesize deletions"
        );
        assert!(next
            .files
            .iter()
            .any(|file| file.logical_root == "$STEAM_LIBRARY:0"));
        assert!(!next
            .files
            .iter()
            .any(|file| file.logical_root == "$STEAM_LIBRARY:1"));
        assert!(
            read_committed_manifest(&storage, &master, &app, source.bundle_id)
                .await
                .is_err(),
            "replacement retires the old cloud generation, not live saves"
        );
        assert!(
            read_committed_manifest(&storage, &master, &app, next.bundle_id)
                .await
                .is_ok()
        );
        let second = StateAgent::boot(AgentConfig::isolated(root.join("second"))).unwrap();
        second.roots.lock().steam_libraries.clear();
        second.roots.lock().steam_libraries.insert(
            "0".into(),
            second.config.home.join("SteamLibrary/steamapps"),
        );
        let second_roots = prepare_restore_roots(&second, &next).unwrap();
        let next_index = read_pack_index(&storage, &master, &app, next.bundle_id)
            .await
            .unwrap();
        let plan = prepare_restore(
            &storage,
            &master,
            &second.config.paths,
            &app,
            next.bundle_id,
            RestoreMode::CompleteApplication,
        )
        .await
        .unwrap();
        restore_pipelined(
            &storage,
            &master,
            &plan,
            &next_index,
            &second_roots,
            None,
            None,
            PipelineOptions::default(),
            |_| Ok(()),
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read(second.config.home.join(".config/external-save/save.dat")).unwrap(),
            b"changed save"
        );
        assert_eq!(
            std::fs::read(
                second
                    .config
                    .home
                    .join("SteamLibrary/steamapps/common/TestGame/game.bin")
            )
            .unwrap(),
            b"game"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn preferred_steamapps_dir_uses_registered_library_first() {
        let mut roots = LogicalRootMap::default();
        roots.steam_root = Some(PathBuf::from("/steam/root"));
        roots
            .steam_libraries
            .insert("0".into(), PathBuf::from("/steam/library/steamapps"));

        assert_eq!(
            preferred_steamapps_dir(&roots),
            Some(PathBuf::from("/steam/library/steamapps"))
        );
    }

    #[test]
    fn clean_machine_restore_derives_proton_prefix_without_registering_scan_root() {
        let home = std::env::temp_dir().join(format!(
            "noland-restore-proton-root-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let agent = StateAgent::boot(AgentConfig::isolated(home.clone())).unwrap();
        let bundle = steam_bundle("Test Game");

        let roots = prepare_restore_roots(&agent, &bundle).unwrap();
        let expected = agent
            .config
            .home
            .join(".steam/steam/steamapps/compatdata/3241660/pfx");

        assert_eq!(roots.proton_prefixes.get(&3_241_660), Some(&expected));
        assert!(!agent
            .db
            .known_roots(Some(&AppId::steam(3_241_660)))
            .unwrap()
            .iter()
            .any(|(_, kind, _)| kind == "proton"));
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn fallback_does_not_synthesize_manifest_from_display_name() {
        let home = std::env::temp_dir().join(format!(
            "noland-restore-steam-manifest-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let steamapps = home.join("steamapps");
        std::fs::create_dir_all(&steamapps).unwrap();

        let agent = StateAgent::boot(AgentConfig::isolated(home.clone())).unwrap();
        agent
            .roots
            .lock()
            .steam_libraries
            .insert("0".into(), steamapps.clone());
        let bundle = steam_bundle("Display Name Is Not The Install Directory");

        assert!(steam_restore_prerequisites(
            &bundle,
            &agent.roots.lock(),
            RestoreMode::CompleteApplication
        )
        .is_empty());

        assert!(!steamapps.join("appmanifest_3241660.acf").exists());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn prerequisite_planning_preserves_original_steam_manifest_and_personal_state() {
        let home = std::env::temp_dir().join(format!(
            "noland-restore-steam-pre-manifest-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let steamapps = home.join("steamapps");
        std::fs::create_dir_all(&steamapps).unwrap();
        let agent = StateAgent::boot(AgentConfig::isolated(home.clone())).unwrap();
        agent
            .roots
            .lock()
            .steam_libraries
            .insert("0".into(), steamapps.clone());
        let mut bundle = steam_bundle("Test Game");
        bundle.app.canonical_executable = Some(PathBuf::from(
            "/source/steamapps/common/TestGame/bin/game.exe",
        ));
        let manifest_path = steamapps.join("appmanifest_3241660.acf");
        std::fs::write(&manifest_path, "old manifest").unwrap();

        let roots = prepare_restore_roots(&agent, &bundle).unwrap();
        let prerequisites =
            steam_restore_prerequisites(&bundle, &roots, RestoreMode::CompleteApplication);
        assert_eq!(prerequisites.len(), 1);
        assert!(
            steam_restore_prerequisites(&bundle, &roots, RestoreMode::PersonalState).is_empty()
        );

        assert_eq!(std::fs::read(&manifest_path).unwrap(), b"old manifest");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn transaction_prerequisite_uses_source_install_directory() {
        let home = std::env::temp_dir().join(format!(
            "noland-restore-steam-source-manifest-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let steamapps = home.join("steamapps");
        std::fs::create_dir_all(&steamapps).unwrap();

        let agent = StateAgent::boot(AgentConfig::isolated(home.clone())).unwrap();
        agent
            .roots
            .lock()
            .steam_libraries
            .insert("0".into(), steamapps.clone());
        let mut bundle = steam_bundle("Unrelated Display Name");
        bundle.app.canonical_executable = Some(PathBuf::from(
            "/source/steamapps/common/ActualInstallDirectory/bin/game.exe",
        ));

        let prerequisites = steam_restore_prerequisites(
            &bundle,
            &agent.roots.lock(),
            RestoreMode::CompleteApplication,
        );

        let manifest = std::str::from_utf8(&prerequisites[0].contents).unwrap();
        assert!(manifest.contains("\"installdir\"\t\t\"ActualInstallDirectory\""));
        assert!(!manifest.contains("\"installdir\"\t\t\"Unrelated Display Name\""));
        std::fs::remove_dir_all(home).unwrap();
    }
}
