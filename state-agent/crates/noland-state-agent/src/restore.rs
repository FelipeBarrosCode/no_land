use std::{fs, path::PathBuf, time::Instant};

use noland_crypto::MasterKey;
use noland_rclone_adapter::{EphemeralRcloneSession, TransferTuning};
use noland_restore::{
    apply_restore_to, cleanup_restore, download_and_verify_to, materialize_tree_to,
    prepare_restore, DownloadJournal, DownloadOptions, DownloadReport, RestoreTarget,
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
    let storage = RcloneStorage::from_session(session, &config_path).with_transfer_tuning(tuning);
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

                ensure_steam_appmanifest(agent, manifest_app)?;
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
                let roots = agent.roots.lock().clone();
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
                    RestoreState::Materializing,
                    &metrics,
                )?;
                persist_restore_progress(
                    agent,
                    operation_id,
                    &mut progress,
                    RestoreState::Materializing.as_str(),
                    0,
                    "Materializing launch-critical files from verified chunks",
                )?;
                let materialize_started = Instant::now();
                let ready_materialized =
                    materialize_tree_to(&plan, &roots, RestoreTarget::ReadyToLaunch)?;
                metrics.restore_materialize_duration_ms = metrics
                    .restore_materialize_duration_ms
                    .saturating_add(elapsed_ms(materialize_started));
                progress.detail_json["ready_to_launch_materialized_files"] =
                    serde_json::json!(ready_materialized.len());

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
                    "Applying launch-critical files with rollback protection",
                )?;
                let apply_started = Instant::now();
                let _ready_rollback =
                    apply_restore_to(&plan, &roots, Some(&agent.db), RestoreTarget::ReadyToLaunch)?;
                metrics.restore_apply_duration_ms = metrics
                    .restore_apply_duration_ms
                    .saturating_add(elapsed_ms(apply_started));

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

                persist_restore_operation(
                    agent,
                    operation_id,
                    RestoreState::Materializing,
                    &metrics,
                )?;
                persist_restore_progress(
                    agent,
                    operation_id,
                    &mut progress,
                    RestoreState::Materializing.as_str(),
                    ready_to_launch_files,
                    "Materializing all remaining restore files",
                )?;
                let materialize_started = Instant::now();
                let complete_materialized =
                    materialize_tree_to(&plan, &roots, RestoreTarget::Complete)?;
                metrics.restore_materialize_duration_ms = metrics
                    .restore_materialize_duration_ms
                    .saturating_add(elapsed_ms(materialize_started));
                progress.detail_json["complete_materialized_files"] =
                    serde_json::json!(complete_materialized.len());

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
                    "Applying the complete restore and deferred tombstones",
                )?;
                let apply_started = Instant::now();
                let rollback =
                    apply_restore_to(&plan, &roots, Some(&agent.db), RestoreTarget::Complete)?;
                metrics.restore_apply_duration_ms = metrics
                    .restore_apply_duration_ms
                    .saturating_add(elapsed_ms(apply_started));
                progress.detail_json["milestones"] =
                    serde_json::json!(["READY_TO_LAUNCH", "COMPLETE"]);
                Ok(rollback)
            }
            .await;

            match restore_result {
                Ok(rollback) => cleanup_restore(&plan, rollback),
                Err(error) => {
                    if let Err(cleanup_error) = cleanup_restore(&plan, None) {
                        tracing::warn!(
                            restore_id = %plan.restore_id,
                            %cleanup_error,
                            "failed to clean restore staging after restore error"
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

fn ensure_steam_appmanifest(agent: &StateAgent, manifest_app: &ManifestApp) -> Result<()> {
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
    if roots
        .steam_libraries
        .values()
        .map(|steamapps| steamapps.join(&filename))
        .any(|path| path.is_file())
    {
        return Ok(());
    }

    let Some(target_dir) = preferred_steamapps_dir(&roots) else {
        return Ok(());
    };
    fs::create_dir_all(target_dir.join("common"))?;
    let manifest_path = target_dir.join(&filename);
    let manifest = format!(
        "\"AppState\"\n{{\n\t\"appid\"\t\t\"{steam_app_id}\"\n\t\"name\"\t\t\"{}\"\n\t\"StateFlags\"\t\t\"4\"\n\t\"installdir\"\t\t\"{}\"\n}}\n",
        escape_acf_value(&manifest_app.display_name),
        escape_acf_value(&steam_install_dir_hint(manifest_app, steam_app_id)),
    );
    fs::write(manifest_path, manifest)?;
    Ok(())
}

fn preferred_steamapps_dir(roots: &LogicalRootMap) -> Option<PathBuf> {
    roots
        .steam_libraries
        .values()
        .next()
        .cloned()
        .or_else(|| roots.steam_root.as_ref().map(|root| root.join("steamapps")))
}

fn steam_install_dir_hint(manifest_app: &ManifestApp, steam_app_id: u32) -> String {
    let candidate = manifest_app.display_name.trim();
    let sanitized = candidate
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, ' ' | '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let trimmed = sanitized.trim_matches([' ', '.']);
    if trimmed.is_empty() {
        format!("Steam-{steam_app_id}")
    } else {
        trimmed.to_string()
    }
}

fn escape_acf_value(value: &str) -> String {
    value.replace('"', "'")
}

#[cfg(test)]
mod tests {
    use super::{ensure_steam_appmanifest, preferred_steamapps_dir, steam_install_dir_hint};
    use crate::{AgentConfig, StateAgent};
    use noland_state_core::{AppId, LogicalRootMap, ManifestApp};
    use std::path::PathBuf;

    #[test]
    fn steam_install_dir_hint_sanitizes_manifest_titles() {
        let app = ManifestApp {
            app_id: AppId::steam(42),
            display_name: "O'Brien: The / Test".into(),
            aliases: Vec::new(),
            desktop_entry_id: None,
            steam_app_id: Some(42),
            launcher: None,
            canonical_executable: None,
            icon_path: None,
        };

        assert_eq!(steam_install_dir_hint(&app, 42), "O_Brien_ The _ Test");
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
    fn ensure_steam_appmanifest_creates_missing_manifest() {
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

        let config = AgentConfig::isolated(home.clone());
        let agent = StateAgent::boot(config).unwrap();
        {
            let mut roots = agent.roots.lock();
            roots.steam_libraries.insert("0".into(), steamapps.clone());
        }

        let app = ManifestApp {
            app_id: AppId::steam(3241660),
            display_name: "R.E.P.O.".into(),
            aliases: Vec::new(),
            desktop_entry_id: None,
            steam_app_id: Some(3_241_660),
            launcher: None,
            canonical_executable: None,
            icon_path: None,
        };

        ensure_steam_appmanifest(&agent, &app).unwrap();

        let manifest = steamapps.join("appmanifest_3241660.acf");
        let text = std::fs::read_to_string(&manifest).unwrap();
        assert!(manifest.is_file());
        assert!(text.contains("\"appid\"\t\t\"3241660\""));
        assert!(text.contains("\"name\"\t\t\"R.E.P.O.\""));
        std::fs::remove_dir_all(home).unwrap();
    }
}
