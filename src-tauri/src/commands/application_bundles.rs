use tauri::State;
use tracing::info;

use crate::errors::{AppError, FrontendError};
use crate::models::{
    app_state::{BundleIndex, RestoreJob, RestoreRequest},
    application_bundle::{
        ApplicationBundle, ApplicationBundleId, ApplicationCatalog, ApplicationCatalogEntry,
        ApplicationKind, BackupStatus, BundleAvailability, BundleComponent, BundleComponentId,
        BundleComponentKind, BundleRelationship, BundleRestoreResult, BundleSnapshot, RestoreMode,
        SnapshotId,
    },
};
use crate::services::{
    app_context::AppContext, remote_exec::RemoteExec,
    shared_storage::bundle_restore::BundleRestoreService, vast_api::VastApiClient,
};

#[tauri::command]
pub async fn list_application_bundles(
    context: State<'_, AppContext>,
    instance_id: Option<u64>,
) -> Result<Vec<ApplicationBundle>, FrontendError> {
    let bundles = load_real_application_bundles(context.inner(), instance_id).await?;
    context
        .update_state(|state| {
            state.application_catalog = bundles.clone();
        })
        .await?;
    Ok(bundles)
}

#[tauri::command]
pub async fn get_application_bundle(
    context: State<'_, AppContext>,
    bundle_id: String,
) -> Result<ApplicationBundle, FrontendError> {
    let state = context.load_state().await;
    Ok(state
        .application_catalog
        .iter()
        .find(|b| b.id.0 == bundle_id)
        .cloned()
        .ok_or_else(|| AppError::NotFound(format!("Bundle {bundle_id} not found")))?)
}

#[tauri::command]
pub async fn list_bundle_snapshots(
    context: State<'_, AppContext>,
    bundle_id: String,
) -> Result<Vec<BundleSnapshot>, FrontendError> {
    let state = context.load_state().await;
    let bundle = state
        .application_catalog
        .iter()
        .find(|b| b.id.0 == bundle_id)
        .cloned()
        .ok_or_else(|| AppError::NotFound(format!("Bundle {bundle_id} not found")))?;

    let snapshot_id = bundle
        .latest_snapshot_id
        .clone()
        .unwrap_or_else(|| "latest".to_string());

    Ok(vec![BundleSnapshot {
        id: SnapshotId(snapshot_id.clone()),
        bundle_id: bundle.id.clone(),
        parent_snapshot_id: None,
        state: "committed".to_string(),
        reason: "indexed-from-shared-storage".to_string(),
        total_logical_size: bundle.components.iter().map(|c| c.logical_size).sum(),
        new_physical_size: 0,
        created_at: bundle.updated_at,
        committed_at: Some(bundle.updated_at),
    }])
}

#[tauri::command]
pub async fn discover_application_bundles(
    context: State<'_, AppContext>,
    instance_id: u64,
) -> Result<Vec<ApplicationBundle>, FrontendError> {
    info!("Discovering application bundles for instance {instance_id}");
    let bundles = load_real_application_bundles(context.inner(), Some(instance_id)).await?;
    context
        .update_state(|state| {
            state.application_catalog = bundles.clone();
        })
        .await?;
    Ok(bundles)
}

#[tauri::command]
pub async fn restore_application_bundle(
    context: State<'_, AppContext>,
    bundle_id: String,
    snapshot_id: Option<String>,
    restore_mode: String,
    selected_components: Option<Vec<String>>,
) -> Result<BundleRestoreResult, FrontendError> {
    let mode = match restore_mode.as_str() {
        "complete" => RestoreMode::Complete,
        "personal_state" => RestoreMode::PersonalState,
        "custom" => RestoreMode::Custom,
        _ => {
            return Err(
                AppError::InvalidInput(format!("Unknown restore mode: {restore_mode}")).into(),
            )
        }
    };

    let instance_id = {
        let state = context.load_state().await;
        state
            .instance
            .instance_id
            .ok_or_else(|| AppError::InvalidInput("No active instance selected.".to_string()))?
    };

    let remote = build_remote_exec_from_state(context.inner(), Some(instance_id)).await?;
    let target_user = context.config.audio_target_user.clone();
    let index =
        BundleRestoreService::list_bundles(context.inner(), &remote, instance_id, &target_user)
            .await?;
    let legacy_bundle = index
        .bundles
        .iter()
        .find(|bundle| bundle.id == bundle_id)
        .ok_or_else(|| AppError::NotFound(format!("Bundle {bundle_id} not found")))?;

    let folder_bundle_ids = match mode {
        RestoreMode::Complete => legacy_bundle
            .folder_bundles
            .iter()
            .map(|folder| folder.id.clone())
            .collect(),
        RestoreMode::PersonalState => legacy_bundle
            .folder_bundles
            .iter()
            .filter(|folder| folder.default_selected)
            .map(|folder| folder.id.clone())
            .collect(),
        RestoreMode::Custom => selected_components.unwrap_or_default(),
    };

    if folder_bundle_ids.is_empty() {
        return Err(AppError::InvalidInput(
            "No bundle components were selected for restore.".to_string(),
        )
        .into());
    }

    let job = BundleRestoreService::restore_bundle(
        context.inner(),
        &remote,
        instance_id,
        &target_user,
        RestoreRequest {
            bundle_id: bundle_id.clone(),
            folder_bundle_ids,
            mode: "merge".to_string(),
        },
    )
    .await?;

    Ok(bundle_restore_result_from_job(
        &job,
        ApplicationBundleId(bundle_id),
        snapshot_id.unwrap_or_else(|| index.snapshot_id.clone()),
    ))
}

#[tauri::command]
pub async fn get_restore_job_status(job_id: String) -> Result<BundleRestoreResult, FrontendError> {
    let job = BundleRestoreService::get_job(&job_id).await?;
    Ok(bundle_restore_result_from_job(
        &job,
        ApplicationBundleId(job.bundle_id.clone()),
        "latest".to_string(),
    ))
}

#[tauri::command]
pub async fn cancel_restore_job(_job_id: String) -> Result<(), FrontendError> {
    Err(
        AppError::InvalidInput("Restore job cancellation is not implemented yet.".to_string())
            .into(),
    )
}

#[tauri::command]
pub async fn remove_local_bundle(
    context: State<'_, AppContext>,
    bundle_id: String,
) -> Result<(), FrontendError> {
    info!("Removing local copy of bundle: {bundle_id} (cloud backup preserved)");

    let bundles = context.load_state().await.application_catalog.clone();
    let updated: Vec<ApplicationBundle> = bundles
        .into_iter()
        .map(|mut b| {
            if b.id.0 == bundle_id && b.availability == BundleAvailability::Installed {
                b.availability = BundleAvailability::CloudOnly;
            }
            b
        })
        .collect();

    context
        .update_state(|state| {
            state.application_catalog = updated;
        })
        .await?;

    Ok(())
}

#[tauri::command]
pub async fn request_cloud_deletion(bundle_id: String) -> Result<(), FrontendError> {
    info!("Cloud deletion requested for bundle: {bundle_id} (delayed GC)");
    Ok(())
}

#[tauri::command]
pub async fn trigger_bundle_backup(_bundle_id: String) -> Result<(), FrontendError> {
    Err(AppError::InvalidInput(
        "Per-bundle backup jobs are not implemented yet. Use the shared storage export flow instead."
            .to_string(),
    )
    .into())
}

#[tauri::command]
pub async fn get_application_catalog(
    context: State<'_, AppContext>,
) -> Result<ApplicationCatalog, FrontendError> {
    let bundles = load_real_application_bundles(context.inner(), None).await?;
    Ok(build_catalog(&bundles))
}

#[tauri::command]
pub async fn reload_application_catalog(
    context: State<'_, AppContext>,
) -> Result<ApplicationCatalog, FrontendError> {
    info!("Reloading application catalog");
    let bundles = load_real_application_bundles(context.inner(), None).await?;
    context
        .update_state(|state| {
            state.application_catalog = bundles.clone();
        })
        .await?;
    Ok(build_catalog(&bundles))
}

async fn load_real_application_bundles(
    context: &AppContext,
    instance_id: Option<u64>,
) -> Result<Vec<ApplicationBundle>, FrontendError> {
    let resolved_instance_id = match instance_id {
        Some(id) => id,
        None => {
            let state = context.load_state().await;
            match state.instance.instance_id {
                Some(id) => id,
                None => return Ok(Vec::new()),
            }
        }
    };

    let remote = build_remote_exec_from_state(context, Some(resolved_instance_id)).await?;
    let target_user = context.config.audio_target_user.clone();
    let index =
        BundleRestoreService::list_bundles(context, &remote, resolved_instance_id, &target_user)
            .await
            .map_err(FrontendError::from)?;
    Ok(convert_bundle_index(index))
}

fn convert_bundle_index(index: BundleIndex) -> Vec<ApplicationBundle> {
    let now = chrono::Utc::now().timestamp();
    index
        .bundles
        .into_iter()
        .map(|bundle| {
            let bundle_id = ApplicationBundleId(bundle.id.clone());
            let components = bundle
                .folder_bundles
                .iter()
                .enumerate()
                .map(|(idx, folder)| BundleComponent {
                    id: BundleComponentId(folder.id.clone()),
                    bundle_id: bundle_id.clone(),
                    kind: bundle_component_kind_from_folder_kind(&folder.kind),
                    display_name: folder.label.clone(),
                    required: matches!(folder.kind.as_str(), "file" | "config"),
                    selected_by_default: folder.default_selected,
                    restore_order: idx as u32,
                    logical_size: 0,
                    file_count: 0,
                    paths: vec![],
                })
                .collect::<Vec<_>>();

            ApplicationBundle {
                id: bundle_id,
                display_name: bundle.name.clone(),
                kind: ApplicationKind::Custom,
                store: None,
                store_application_id: None,
                store_metadata: None,
                icon_object_key: None,
                components,
                relationships: Vec::<BundleRelationship>::new(),
                availability: BundleAvailability::CloudOnly,
                backup_status: BackupStatus::Committed,
                latest_snapshot_id: Some(index.snapshot_id.clone()),
                created_at: now,
                updated_at: now,
            }
        })
        .collect()
}

fn build_catalog(bundles: &[ApplicationBundle]) -> ApplicationCatalog {
    ApplicationCatalog {
        repository_id: "shared-storage-active-profile".to_string(),
        generation: 1,
        generated_at: chrono::Utc::now().timestamp(),
        bundles: bundles
            .iter()
            .map(|bundle| ApplicationCatalogEntry {
                bundle_id: bundle.id.clone(),
                display_name: bundle.display_name.clone(),
                kind: bundle.kind.clone(),
                icon_object_key: bundle.icon_object_key.clone(),
                latest_snapshot_id: bundle.latest_snapshot_id.clone(),
                total_logical_size: bundle.components.iter().map(|c| c.logical_size).sum(),
                required_restore_size: bundle
                    .components
                    .iter()
                    .filter(|c| c.required)
                    .map(|c| c.logical_size)
                    .sum(),
                last_backed_up_at: bundle.updated_at,
                component_summary: crate::models::application_bundle::ComponentSummary {
                    has_application_content: bundle
                        .components
                        .iter()
                        .any(|c| matches!(c.kind, BundleComponentKind::ApplicationContent)),
                    has_saves: bundle
                        .components
                        .iter()
                        .any(|c| matches!(c.kind, BundleComponentKind::SaveFiles)),
                    has_mods: bundle.components.iter().any(|c| {
                        matches!(
                            c.kind,
                            BundleComponentKind::Mods | BundleComponentKind::Modifications
                        )
                    }),
                    has_runtime: bundle.components.iter().any(|c| {
                        matches!(
                            c.kind,
                            BundleComponentKind::RuntimePrefix | BundleComponentKind::WinePrefix
                        )
                    }),
                    has_configuration: bundle.components.iter().any(|c| {
                        matches!(
                            c.kind,
                            BundleComponentKind::UserConfiguration
                                | BundleComponentKind::UserProfile
                        )
                    }),
                    has_plugins: bundle
                        .components
                        .iter()
                        .any(|c| matches!(c.kind, BundleComponentKind::Plugins)),
                    component_count: bundle.components.len() as u32,
                },
            })
            .collect(),
    }
}

fn bundle_component_kind_from_folder_kind(kind: &str) -> BundleComponentKind {
    match kind {
        "file" => BundleComponentKind::SaveFiles,
        "config" => BundleComponentKind::UserConfiguration,
        "data" => BundleComponentKind::UserProfile,
        "mods" => BundleComponentKind::Mods,
        "plugin" => BundleComponentKind::Plugins,
        _ => BundleComponentKind::Custom(kind.to_string()),
    }
}

fn bundle_restore_result_from_job(
    job: &RestoreJob,
    bundle_id: ApplicationBundleId,
    snapshot_id: String,
) -> BundleRestoreResult {
    let completed_components = job
        .items
        .iter()
        .filter(|item| item.status == "completed")
        .count() as u32;
    BundleRestoreResult {
        job_id: job.job_id.clone(),
        bundle_id,
        snapshot_id,
        status: job.status.clone(),
        total_components: job.items.len() as u32,
        completed_components,
        total_files: 0,
        restored_files: 0,
        total_bytes: 0,
        restored_bytes: 0,
        started_at: job.started_at.clone(),
        finished_at: job.finished_at.clone(),
        error: job.error.clone(),
    }
}

async fn build_remote_exec_from_state(
    context: &AppContext,
    explicit_instance_id: Option<u64>,
) -> Result<RemoteExec, FrontendError> {
    let state = context.load_state().await;
    let private_key_path = state.ssh.private_key_path.clone();
    if private_key_path.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "SSH private key path is empty. Run provisioning first.".to_string(),
        )
        .into());
    }

    let instance_id = explicit_instance_id
        .or(state.instance.instance_id)
        .ok_or_else(|| AppError::InvalidInput("No active instance selected.".to_string()))?;

    let ssh_user = if state.ssh.ssh_username.trim().is_empty() {
        context.config.ssh_user.clone()
    } else {
        state.ssh.ssh_username.clone()
    };

    let api_key = state.credentials.vast_api_key.clone();
    if api_key.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "Vast API key is missing. Add it in Settings first.".to_string(),
        )
        .into());
    }
    drop(state);

    let vast = VastApiClient::new(
        context.http_client.clone(),
        context.config.vast_base_url.clone(),
        api_key,
    );
    let instance = vast
        .get_instance(instance_id)
        .await
        .map_err(FrontendError::from)?;
    let ssh_host = if instance.public_ip.trim().is_empty() {
        instance.ssh_host.clone()
    } else {
        instance.public_ip.clone()
    };
    if ssh_host.trim().is_empty() || instance.ssh_port == 0 {
        return Err(AppError::InvalidInput(format!(
            "Instance {} SSH details are unavailable.",
            instance_id
        ))
        .into());
    }

    Ok(RemoteExec {
        ssh_user,
        ssh_host,
        ssh_port: instance.ssh_port,
        private_key_path,
    })
}
