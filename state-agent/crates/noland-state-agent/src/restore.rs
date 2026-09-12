use std::{
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

use noland_crypto::MasterKey;
use noland_rclone_adapter::{EphemeralRcloneSession, TransferTuning};
use noland_restore::{
    download_and_verify_to, prepare_restore, DownloadJournal, DownloadOptions, DownloadReport,
    RestoreTarget, RestoreTransaction,
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

fn download_report_json(report: DownloadReport) -> serde_json::Value {
    serde_json::json!({
        "packs_downloaded": report.packs_downloaded,
        "packs_reused": report.packs_reused,
        "chunks_extracted": report.chunks_extracted,
        "chunks_reused": report.chunks_reused,
    })
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
    let tuning = if agent.db.open_session_app_ids()?.is_empty() {
        TransferTuning::throughput()
    } else {
        TransferTuning::gameplay_safe()
    };
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
                let mut restore = RestoreTransaction::new(&plan, &roots, Some(&agent.db));
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
                crate::backup::remember_remote_pack_index(
                    agent,
                    storage.storage_identity(),
                    &index,
                )?;
                // Restore retries reuse verified local pack/chunk caches directly. Their old
                // journal rows are not needed for resume and would pollute this attempt's totals.
                agent.db.delete_sync_journal_entries_for_kind_direction(
                    operation_id,
                    ContentObjectKind::Pack,
                    SyncDirection::Download,
                )?;
                let total_packs = noland_restore::planned_pack_download_count(
                    &plan,
                    &index,
                    RestoreTarget::Complete,
                )?;
                progress.detail_json["total_packs"] = serde_json::json!(total_packs);
                let download_journal = Some(DownloadJournal {
                    db: &agent.db,
                    operation_id,
                });

                persist_restore_operation(
                    agent,
                    operation_id,
                    RestoreState::Downloading,
                    &metrics,
                )?;
                persist_restore_progress(
                    agent,
                    operation_id,
                    &mut progress,
                    RestoreState::Downloading.as_str(),
                    0,
                    "Downloading launch-critical packs with cache and resume support",
                )?;
                let download_started = Instant::now();
                let ready_download = download_and_verify_to(
                    &storage,
                    master,
                    &plan,
                    &index,
                    RestoreTarget::ReadyToLaunch,
                    DownloadOptions::default(),
                    download_journal,
                )
                .await?;
                metrics.download_duration_ms = metrics
                    .download_duration_ms
                    .saturating_add(elapsed_ms(download_started));
                progress.detail_json["ready_to_launch_download"] =
                    download_report_json(ready_download);

                persist_restore_operation(
                    agent,
                    operation_id,
                    RestoreState::CreatingRollbackPoint,
                    &metrics,
                )?;
                persist_restore_operation(agent, operation_id, RestoreState::Applying, &metrics)?;
                persist_restore_progress(
                    agent,
                    operation_id,
                    &mut progress,
                    RestoreState::Applying.as_str(),
                    0,
                    "Reconstructing and atomically publishing launch-critical files",
                )?;
                let apply_started = Instant::now();
                let ready_report = restore.publish_to(RestoreTarget::ReadyToLaunch)?;
                metrics.restore_apply_duration_ms = metrics
                    .restore_apply_duration_ms
                    .saturating_add(elapsed_ms(apply_started));
                progress.detail_json["ready_to_launch_published_files"] =
                    serde_json::json!(ready_report.published_entries);
                progress.detail_json["ready_to_launch_reused_files"] =
                    serde_json::json!(ready_report.reused_entries);

                progress.detail_json["ready_to_launch_reached"] = serde_json::json!(true);
                progress.detail_json["milestones"] = serde_json::json!(["READY_TO_LAUNCH"]);
                persist_restore_progress(
                    agent,
                    operation_id,
                    &mut progress,
                    "READY_TO_LAUNCH",
                    ready_to_launch_files,
                    "Launch-critical restore data is ready; prefetching soon-needed packs",
                )?;

                persist_restore_progress(
                    agent,
                    operation_id,
                    &mut progress,
                    RestoreState::Downloading.as_str(),
                    ready_to_launch_files,
                    "Prefetching the soon-needed restore tier from cached and remote packs",
                )?;
                let prefetch_started = Instant::now();
                let soon_download = download_and_verify_to(
                    &storage,
                    master,
                    &plan,
                    &index,
                    RestoreTarget::Soon,
                    DownloadOptions::default(),
                    download_journal,
                )
                .await?;
                metrics.download_duration_ms = metrics
                    .download_duration_ms
                    .saturating_add(elapsed_ms(prefetch_started));
                progress.detail_json["soon_prefetch_download"] =
                    download_report_json(soon_download);
                progress.detail_json["milestones"] =
                    serde_json::json!(["READY_TO_LAUNCH", "SOON_PREFETCH"]);

                progress.detail_json["target"] = serde_json::json!("COMPLETE");
                persist_restore_operation(
                    agent,
                    operation_id,
                    RestoreState::Downloading,
                    &metrics,
                )?;
                persist_restore_progress(
                    agent,
                    operation_id,
                    &mut progress,
                    RestoreState::Downloading.as_str(),
                    ready_to_launch_files,
                    "Resuming from verified chunks and cached packs for the complete restore",
                )?;
                let download_started = Instant::now();
                let complete_download = download_and_verify_to(
                    &storage,
                    master,
                    &plan,
                    &index,
                    RestoreTarget::Complete,
                    DownloadOptions::default(),
                    download_journal,
                )
                .await?;
                metrics.download_duration_ms = metrics
                    .download_duration_ms
                    .saturating_add(elapsed_ms(download_started));
                progress.detail_json["complete_download"] = download_report_json(complete_download);

                let staged_packs_released = restore.release_staged_pack_links()?;
                let pack_cache_pruned = restore.prune_restore_pack_cache()?;
                let chunks_evicted_on_enable = restore.enable_chunk_eviction()?;
                progress.detail_json["final_publication_cleanup"] = serde_json::json!({
                    "staged_pack_links_released": staged_packs_released,
                    "cached_packs_pruned": pack_cache_pruned.packs_pruned,
                    "cached_pack_bytes_before": pack_cache_pruned.bytes_before,
                    "chunks_evicted_on_enable": chunks_evicted_on_enable,
                });

                persist_restore_operation(
                    agent,
                    operation_id,
                    RestoreState::CreatingRollbackPoint,
                    &metrics,
                )?;
                persist_restore_operation(agent, operation_id, RestoreState::Applying, &metrics)?;
                persist_restore_progress(
                    agent,
                    operation_id,
                    &mut progress,
                    RestoreState::Applying.as_str(),
                    ready_to_launch_files,
                    "Reconstructing and atomically publishing all remaining files and tombstones",
                )?;
                let apply_started = Instant::now();
                let complete_report = restore.publish_to(RestoreTarget::Complete)?;
                metrics.restore_apply_duration_ms = metrics
                    .restore_apply_duration_ms
                    .saturating_add(elapsed_ms(apply_started));
                progress.detail_json["complete_published_files"] =
                    serde_json::json!(complete_report.published_entries);
                progress.detail_json["complete_reused_files"] =
                    serde_json::json!(complete_report.reused_entries);
                progress.detail_json["milestones"] =
                    serde_json::json!(["READY_TO_LAUNCH", "COMPLETE"]);
                restore.commit()?;
                if mode == RestoreMode::CompleteApplication {
                    ensure_steam_appmanifest_after_commit(agent, &plan.manifest)?;
                }
                Ok(())
            }
            .await;

            let terminal_pack_release = plan.release_staged_pack_links();
            let terminal_pack_prune = plan.prune_restore_pack_cache();
            let terminal_pack_cleanup = terminal_pack_release.and(terminal_pack_prune);
            match restore_result {
                Ok(()) => {
                    let report = terminal_pack_cleanup?;
                    progress.detail_json["terminal_pack_cache_cleanup"] = serde_json::json!({
                        "packs_pruned": report.packs_pruned,
                        "bytes_before": report.bytes_before,
                        "bytes_after": report.bytes_after,
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

fn ensure_steam_appmanifest_after_commit(
    agent: &StateAgent,
    bundle: &BundleManifest,
) -> Result<()> {
    let manifest_app = &bundle.app;
    let Some(steam_app_id) = manifest_app.steam_app_id.or_else(|| {
        manifest_app
            .app_id
            .as_str()
            .strip_prefix("steam:")
            .and_then(|value| value.parse::<u32>().ok())
    }) else {
        return Ok(());
    };

    let roots = agent.roots.lock().clone();
    let filename = format!("appmanifest_{steam_app_id}.acf");
    let Some((target_dir, install_dir)) = steam_install_location_from_manifest(bundle, &roots)
    else {
        return Ok(());
    };
    let manifest_path = target_dir.join(&filename);
    if manifest_path.is_file() {
        return Ok(());
    }

    fs::create_dir_all(&target_dir)?;
    let manifest = format!(
        "\"AppState\"\n{{\n\t\"appid\"\t\t\"{steam_app_id}\"\n\t\"name\"\t\t\"{}\"\n\t\"StateFlags\"\t\t\"4\"\n\t\"installdir\"\t\t\"{}\"\n}}\n",
        escape_acf_value(&manifest_app.display_name),
        escape_acf_value(&install_dir),
    );
    fs::write(manifest_path, manifest)?;
    Ok(())
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
        ensure_steam_appmanifest_after_commit, preferred_steamapps_dir, prepare_restore_roots,
        steam_install_dir_from_path,
    };
    use crate::{AgentConfig, StateAgent};
    use chrono::Utc;
    use noland_state_core::{
        AppId, AppIdentity, BackupMode, BundleManifest, LogicalRootMap, ManifestApp, ManifestSource,
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

        ensure_steam_appmanifest_after_commit(&agent, &bundle).unwrap();

        assert!(!steamapps.join("appmanifest_3241660.acf").exists());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn post_commit_fallback_uses_source_install_directory() {
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

        ensure_steam_appmanifest_after_commit(&agent, &bundle).unwrap();

        let manifest = std::fs::read_to_string(steamapps.join("appmanifest_3241660.acf")).unwrap();
        assert!(manifest.contains("\"installdir\"\t\t\"ActualInstallDirectory\""));
        assert!(!manifest.contains("\"installdir\"\t\t\"Unrelated Display Name\""));
        std::fs::remove_dir_all(home).unwrap();
    }
}
