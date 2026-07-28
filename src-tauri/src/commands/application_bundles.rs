use std::collections::{HashMap, HashSet};
use std::time::Duration;

use tauri::State;
use tracing::info;

use crate::errors::{AppError, FrontendError};
use crate::models::{
    app_state::{
        BundleIndex, BundleLearningSuggestion, BundleTrackingSession, FolderBundle,
        ManualBundleAttachment, RestoreJob, RestoreRequest,
    },
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
pub async fn list_bundle_manual_attachments(
    context: State<'_, AppContext>,
    bundle_id: String,
) -> Result<Vec<FolderBundle>, FrontendError> {
    let state = context.load_state().await;
    Ok(state
        .bundle_learning
        .manual_attachments
        .iter()
        .filter(|attachment| attachment.bundle_id == bundle_id)
        .map(|attachment| attachment.folder_bundle.clone())
        .collect())
}

#[tauri::command]
pub async fn list_bundle_tracking_sessions(
    context: State<'_, AppContext>,
    bundle_id: Option<String>,
) -> Result<Vec<BundleTrackingSession>, FrontendError> {
    let state = context.load_state().await;
    Ok(state
        .bundle_learning
        .tracking_sessions
        .iter()
        .filter(|session| {
            bundle_id
                .as_ref()
                .map(|id| id == &session.bundle_id)
                .unwrap_or(true)
        })
        .cloned()
        .collect())
}

#[tauri::command]
pub async fn start_bundle_tracking(
    context: State<'_, AppContext>,
    bundle_id: String,
) -> Result<BundleTrackingSession, FrontendError> {
    let state = context.load_state().await;
    let instance_id = state
        .instance
        .instance_id
        .ok_or_else(|| AppError::InvalidInput("No active instance selected.".to_string()))?;

    if let Some(existing) = state
        .bundle_learning
        .tracking_sessions
        .iter()
        .find(|session| session.bundle_id == bundle_id && session.instance_id == instance_id)
        .cloned()
    {
        return Ok(existing);
    }

    let session = BundleTrackingSession {
        session_id: uuid::Uuid::new_v4().to_string(),
        bundle_id: bundle_id.clone(),
        instance_id,
        started_at: chrono::Utc::now().to_rfc3339(),
        started_at_epoch: chrono::Utc::now().timestamp(),
    };

    context
        .update_state(|state| {
            state
                .bundle_learning
                .tracking_sessions
                .retain(|existing| existing.bundle_id != bundle_id);
            state
                .bundle_learning
                .tracking_sessions
                .push(session.clone());
        })
        .await?;

    Ok(session)
}

#[tauri::command]
pub async fn stop_bundle_tracking(
    context: State<'_, AppContext>,
    bundle_id: Option<String>,
    session_id: Option<String>,
) -> Result<Vec<BundleLearningSuggestion>, FrontendError> {
    let tracked_session = {
        let state = context.load_state().await;
        state
            .bundle_learning
            .tracking_sessions
            .iter()
            .find(|session| {
                session_id
                    .as_ref()
                    .map(|id| id == &session.session_id)
                    .unwrap_or(false)
                    || bundle_id
                        .as_ref()
                        .map(|id| id == &session.bundle_id)
                        .unwrap_or(false)
            })
            .cloned()
            .ok_or_else(|| {
                AppError::NotFound("No active bundle tracking session was found.".to_string())
            })?
    };

    let remote =
        build_remote_exec_from_state(context.inner(), Some(tracked_session.instance_id)).await?;
    let target_user = context.config.audio_target_user.clone();
    let index = BundleRestoreService::list_bundles(
        context.inner(),
        &remote,
        tracked_session.instance_id,
        &target_user,
    )
    .await?;
    let bundle = index
        .bundles
        .iter()
        .find(|bundle| bundle.id == tracked_session.bundle_id)
        .ok_or_else(|| {
            AppError::NotFound(format!("Bundle {} not found", tracked_session.bundle_id))
        })?;

    let candidate_roots = learning_candidate_roots(bundle, &index.host.home);
    let changed_paths = capture_changed_paths_since(
        &remote,
        &candidate_roots,
        tracked_session.started_at_epoch,
        &index.host.home,
    )
    .await?;
    let suggestions =
        build_learning_suggestions(&tracked_session, &changed_paths, bundle, &index.host.home);

    context
        .update_state(|state| {
            state
                .bundle_learning
                .tracking_sessions
                .retain(|session| session.session_id != tracked_session.session_id);
            state
                .bundle_learning
                .suggestions
                .retain(|suggestion| suggestion.bundle_id != tracked_session.bundle_id);
            state
                .bundle_learning
                .suggestions
                .extend(suggestions.clone());
        })
        .await?;

    Ok(suggestions)
}

#[tauri::command]
pub async fn list_bundle_learning_suggestions(
    context: State<'_, AppContext>,
    bundle_id: String,
) -> Result<Vec<BundleLearningSuggestion>, FrontendError> {
    let state = context.load_state().await;
    Ok(state
        .bundle_learning
        .suggestions
        .iter()
        .filter(|suggestion| suggestion.bundle_id == bundle_id)
        .cloned()
        .collect())
}

#[tauri::command]
pub async fn accept_bundle_learning_suggestion(
    context: State<'_, AppContext>,
    bundle_id: String,
    path: String,
    label: Option<String>,
    kind: Option<String>,
) -> Result<(), FrontendError> {
    let target_user = context.config.audio_target_user.clone();
    let normalized_path = normalize_manual_attachment_path(&path, &target_user)?;

    context
        .update_state(|state| {
            let suggested = state
                .bundle_learning
                .suggestions
                .iter()
                .find(|suggestion| {
                    suggestion.bundle_id == bundle_id
                        && suggestion.folder_bundle.target == normalized_path
                })
                .cloned();

            let folder_bundle = suggested
                .map(|suggestion| {
                    let mut folder_bundle = suggestion.folder_bundle;
                    if let Some(custom_label) =
                        label.as_ref().filter(|value| !value.trim().is_empty())
                    {
                        folder_bundle.label = custom_label.trim().to_string();
                    }
                    if let Some(custom_kind) =
                        kind.as_ref().filter(|value| !value.trim().is_empty())
                    {
                        folder_bundle.kind = custom_kind.trim().to_string();
                    }
                    folder_bundle
                })
                .unwrap_or_else(|| FolderBundle {
                    id: manual_attachment_id(&normalized_path),
                    label: label
                        .as_ref()
                        .map(|value| value.trim().to_string())
                        .filter(|value| !value.is_empty())
                        .unwrap_or_else(|| manual_attachment_label(&normalized_path)),
                    source: normalized_path.trim_start_matches('/').to_string(),
                    target: normalized_path.clone(),
                    kind: kind
                        .as_ref()
                        .map(|value| value.trim().to_string())
                        .filter(|value| !value.is_empty())
                        .unwrap_or_else(|| "folder".to_string()),
                    default_selected: true,
                });

            state.bundle_learning.suggestions.retain(|suggestion| {
                !(suggestion.bundle_id == bundle_id
                    && suggestion.folder_bundle.target == normalized_path)
            });
            state
                .bundle_learning
                .manual_attachments
                .retain(|attachment| {
                    !(attachment.bundle_id == bundle_id
                        && attachment.folder_bundle.target == normalized_path)
                });
            state
                .bundle_learning
                .manual_attachments
                .push(ManualBundleAttachment {
                    bundle_id: bundle_id.clone(),
                    folder_bundle,
                });
        })
        .await?;

    Ok(())
}

#[tauri::command]
pub async fn reject_bundle_learning_suggestion(
    context: State<'_, AppContext>,
    bundle_id: String,
    path: String,
) -> Result<(), FrontendError> {
    let target_user = context.config.audio_target_user.clone();
    let normalized_path = normalize_manual_attachment_path(&path, &target_user)?;
    context
        .update_state(|state| {
            state.bundle_learning.suggestions.retain(|suggestion| {
                !(suggestion.bundle_id == bundle_id
                    && suggestion.folder_bundle.target == normalized_path)
            });
        })
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn attach_path_to_bundle(
    context: State<'_, AppContext>,
    bundle_id: String,
    path: String,
    label: Option<String>,
    kind: Option<String>,
    default_selected: Option<bool>,
) -> Result<(), FrontendError> {
    let target_user = context.config.audio_target_user.clone();
    let normalized_path = normalize_manual_attachment_path(&path, &target_user)?;
    let folder_bundle = FolderBundle {
        id: manual_attachment_id(&normalized_path),
        label: label.unwrap_or_else(|| manual_attachment_label(&normalized_path)),
        source: normalized_path.trim_start_matches('/').to_string(),
        target: normalized_path.clone(),
        kind: kind.unwrap_or_else(|| "folder".to_string()),
        default_selected: default_selected.unwrap_or(true),
    };

    context
        .update_state(|state| {
            state
                .bundle_learning
                .manual_attachments
                .retain(|attachment| {
                    !(attachment.bundle_id == bundle_id
                        && attachment.folder_bundle.target == normalized_path)
                });
            state
                .bundle_learning
                .manual_attachments
                .push(ManualBundleAttachment {
                    bundle_id: bundle_id.clone(),
                    folder_bundle: folder_bundle.clone(),
                });
        })
        .await?;

    Ok(())
}

#[tauri::command]
pub async fn detach_path_from_bundle(
    context: State<'_, AppContext>,
    bundle_id: String,
    path: String,
) -> Result<(), FrontendError> {
    let target_user = context.config.audio_target_user.clone();
    let normalized_path = normalize_manual_attachment_path(&path, &target_user)?;
    context
        .update_state(|state| {
            state
                .bundle_learning
                .manual_attachments
                .retain(|attachment| {
                    !(attachment.bundle_id == bundle_id
                        && attachment.folder_bundle.target == normalized_path)
                });
        })
        .await?;
    Ok(())
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
    let mut index =
        BundleRestoreService::list_bundles(context, &remote, resolved_instance_id, &target_user)
            .await
            .map_err(FrontendError::from)?;
    auto_learn_bundle_attachments(
        context,
        &remote,
        resolved_instance_id,
        &target_user,
        &mut index,
    )
    .await?;
    Ok(convert_bundle_index(index))
}

async fn auto_learn_bundle_attachments(
    context: &AppContext,
    remote: &RemoteExec,
    instance_id: u64,
    _target_user: &str,
    index: &mut BundleIndex,
) -> Result<(), FrontendError> {
    let now_epoch = chrono::Utc::now().timestamp();
    let since_epoch = {
        let state = context.load_state().await;
        state
            .bundle_learning
            .last_passive_scan_epoch
            .unwrap_or(now_epoch - 60 * 60 * 24 * 7)
    };

    let candidate_roots = learning_candidate_roots_for_index(index, &index.host.home);
    let changed_paths =
        capture_changed_paths_since(remote, &candidate_roots, since_epoch, &index.host.home)
            .await?;
    let auto_attachments = build_auto_attachments(index, &changed_paths, &index.host.home);

    context
        .update_state(|state| {
            for attachment in &auto_attachments {
                state.bundle_learning.manual_attachments.retain(|existing| {
                    !(existing.bundle_id == attachment.bundle_id
                        && existing.folder_bundle.target == attachment.folder_bundle.target)
                });
                state
                    .bundle_learning
                    .manual_attachments
                    .push(attachment.clone());
            }
            state.bundle_learning.last_passive_scan_epoch = Some(now_epoch);
        })
        .await?;

    for attachment in &auto_attachments {
        if let Some(bundle) = index
            .bundles
            .iter_mut()
            .find(|bundle| bundle.id == attachment.bundle_id)
        {
            let exists = bundle.folder_bundles.iter().any(|existing| {
                existing.id == attachment.folder_bundle.id
                    || existing.target == attachment.folder_bundle.target
            });
            if !exists {
                bundle.folder_bundles.push(attachment.folder_bundle.clone());
                if !bundle.signals.iter().any(|signal| signal == "auto_learned") {
                    bundle.signals.push("auto_learned".to_string());
                }
                bundle.confidence = bundle.confidence.max(0.97);
            }
        }
    }

    let _ = instance_id;
    Ok(())
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct BundlePathChange {
    path: String,
    kind: String,
    modified_at: i64,
    size: u64,
}

fn learning_candidate_roots_for_index(index: &BundleIndex, home: &str) -> Vec<String> {
    let mut roots = vec![
        format!("{home}/.config"),
        format!("{home}/.local/share"),
        format!("{home}/.local/state"),
        format!("{home}/.cache"),
        format!("{home}/Documents"),
        format!("{home}/Games"),
        format!("{home}/Applications"),
        format!("{home}/Downloads"),
        format!("{home}/.steam"),
        format!("{home}/.wine"),
        format!("{home}/.var/app"),
        format!("{home}/.local/share/Steam"),
        format!("{home}/.local/share/Steam/userdata"),
        format!("{home}/.local/share/lutris"),
        format!("{home}/.config/lutris"),
        format!("{home}/.config/heroic"),
        format!("{home}/Games/Heroic"),
        format!("{home}/Games/Heroic/Prefixes"),
    ];

    for bundle in &index.bundles {
        roots.extend(
            bundle
                .folder_bundles
                .iter()
                .map(|folder| folder.target.clone()),
        );
    }

    let mut deduped = Vec::new();
    let mut seen = HashSet::new();
    for root in roots {
        let normalized = root.trim_end_matches('/').to_string();
        if !normalized.is_empty() && seen.insert(normalized.clone()) {
            deduped.push(normalized);
        }
    }
    deduped
}

fn learning_candidate_roots(
    bundle: &crate::models::app_state::AppBundle,
    home: &str,
) -> Vec<String> {
    let mut roots = vec![
        format!("{home}/.config"),
        format!("{home}/.local/share"),
        format!("{home}/.cache"),
        format!("{home}/Documents"),
        format!("{home}/Games"),
        format!("{home}/Applications"),
        format!("{home}/Downloads"),
        format!("{home}/.steam"),
        format!("{home}/.var/app"),
    ];

    roots.extend(
        bundle
            .folder_bundles
            .iter()
            .map(|folder| folder.target.clone()),
    );

    let mut deduped = Vec::new();
    let mut seen = HashSet::new();
    for root in roots {
        let normalized = root.trim_end_matches('/').to_string();
        if !normalized.is_empty() && seen.insert(normalized.clone()) {
            deduped.push(normalized);
        }
    }
    deduped
}

async fn capture_changed_paths_since(
    remote: &RemoteExec,
    candidate_roots: &[String],
    since_epoch: i64,
    home: &str,
) -> Result<Vec<BundlePathChange>, FrontendError> {
    let roots_json = serde_json::to_string(candidate_roots)
        .map_err(|error| AppError::Command(format!("Failed to encode tracking roots: {error}")))?;
    let script = format!(
        "python3 - <<'PY'\nimport json, os\nroots = json.loads({roots_json:?})\nthreshold = int({since_epoch})\nhome = {home:?}\nresults = []\nseen = set()\nfor root in roots:\n    if not root or not os.path.exists(root):\n        continue\n    stack = [root]\n    while stack:\n        path = stack.pop()\n        if path in seen:\n            continue\n        seen.add(path)\n        try:\n            st = os.lstat(path)\n        except OSError:\n            continue\n        is_dir = os.path.isdir(path)\n        if path.startswith(home) and int(st.st_mtime) >= threshold:\n            results.append({{'path': path, 'kind': 'directory' if is_dir else 'file', 'modified_at': int(st.st_mtime), 'size': 0 if is_dir else int(st.st_size)}})\n        if is_dir:\n            try:\n                with os.scandir(path) as entries:\n                    for entry in entries:\n                        stack.append(entry.path)\n            except OSError:\n                pass\nprint(json.dumps(results))\nPY"
    );

    let output = {
        let remote = remote.clone();
        tokio::task::spawn_blocking(move || remote.ssh(&script, Duration::from_secs(180)))
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))??
    };

    if output.status_code != 0 {
        return Err(AppError::Command(format!(
            "Failed to scan changed bundle paths: {}",
            output.stderr.trim()
        ))
        .into());
    }

    serde_json::from_str::<Vec<BundlePathChange>>(&output.stdout).map_err(|error| {
        AppError::Command(format!("Failed to parse changed bundle paths: {error}")).into()
    })
}

fn build_learning_suggestions(
    tracked_session: &BundleTrackingSession,
    changed_paths: &[BundlePathChange],
    bundle: &crate::models::app_state::AppBundle,
    home: &str,
) -> Vec<BundleLearningSuggestion> {
    let existing_targets: HashSet<String> = bundle
        .folder_bundles
        .iter()
        .map(|folder| folder.target.trim_end_matches('/').to_string())
        .collect();
    let mut suggestions_by_target: HashMap<String, BundleLearningSuggestion> = HashMap::new();

    for change in changed_paths {
        let normalized_change_path = change.path.trim_end_matches('/');
        if existing_targets.iter().any(|existing| {
            normalized_change_path == existing
                || normalized_change_path.starts_with(&format!("{existing}/"))
        }) {
            continue;
        }

        let Some(target) = derive_learning_attachment_root(&change.path, home) else {
            continue;
        };
        if existing_targets.contains(&target) {
            continue;
        }
        suggestions_by_target
            .entry(target.clone())
            .or_insert_with(|| BundleLearningSuggestion {
                bundle_id: tracked_session.bundle_id.clone(),
                folder_bundle: FolderBundle {
                    id: manual_attachment_id(&target),
                    label: manual_attachment_label(&target),
                    source: target.trim_start_matches('/').to_string(),
                    target: target.clone(),
                    kind: "folder".to_string(),
                    default_selected: true,
                },
                discovered_at: chrono::Utc::now().to_rfc3339(),
                source: format!("runtime_learning:{}", change.kind),
                tracking_session_id: Some(tracked_session.session_id.clone()),
            });
    }

    let mut suggestions: Vec<_> = suggestions_by_target.into_values().collect();
    suggestions.sort_by(|left, right| left.folder_bundle.target.cmp(&right.folder_bundle.target));
    suggestions
}

fn build_auto_attachments(
    index: &BundleIndex,
    changed_paths: &[BundlePathChange],
    home: &str,
) -> Vec<ManualBundleAttachment> {
    let mut attachments = Vec::new();
    let mut seen_targets = HashSet::new();
    let existing_targets: HashSet<String> = index
        .bundles
        .iter()
        .flat_map(|bundle| {
            bundle
                .folder_bundles
                .iter()
                .map(|folder| folder.target.trim_end_matches('/').to_string())
        })
        .collect();

    for change in changed_paths {
        let normalized_change_path = change.path.trim_end_matches('/');
        if existing_targets.iter().any(|existing| {
            normalized_change_path == existing
                || normalized_change_path.starts_with(&format!("{existing}/"))
        }) {
            continue;
        }

        let Some(target) = derive_learning_attachment_root(&change.path, home) else {
            continue;
        };
        if existing_targets.contains(&target) || !seen_targets.insert(target.clone()) {
            continue;
        }

        let Some(bundle_id) = select_bundle_for_learned_target(index, &target, home) else {
            continue;
        };

        let kind = infer_auto_attachment_kind(&target);
        attachments.push(ManualBundleAttachment {
            bundle_id,
            folder_bundle: FolderBundle {
                id: manual_attachment_id(&target),
                label: manual_attachment_label(&target),
                source: target.trim_start_matches('/').to_string(),
                target,
                kind,
                default_selected: true,
            },
        });
    }

    attachments
}

fn select_bundle_for_learned_target(
    index: &BundleIndex,
    target: &str,
    home: &str,
) -> Option<String> {
    let target_base = std::path::Path::new(target)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(target);
    let normalized_target_base = normalize_name(target_base);
    let target_relative = target
        .strip_prefix(home)
        .unwrap_or(target)
        .trim_start_matches('/');
    let normalized_target_relative = normalize_name(target_relative);

    let mut best_match: Option<(String, i32)> = None;
    for bundle in &index.bundles {
        let mut score = 0i32;
        let normalized_bundle_name = normalize_name(&bundle.name);
        let normalized_bundle_id = normalize_name(&bundle.id);
        let bundle_tokens = name_tokens(&bundle.name);

        if !normalized_target_base.is_empty() {
            if normalized_bundle_name == normalized_target_base {
                score += 12;
            } else if normalized_bundle_name.contains(&normalized_target_base)
                || normalized_target_base.contains(&normalized_bundle_name)
            {
                score += 8;
            }
            if normalized_bundle_id.contains(&normalized_target_base) {
                score += 5;
            }
        }

        if !normalized_target_relative.is_empty() {
            if normalized_target_relative.contains(&normalized_bundle_name) {
                score += 10;
            }
            if normalized_target_relative.contains(&normalized_bundle_id) {
                score += 6;
            }
            let matched_tokens = bundle_tokens
                .iter()
                .filter(|token| token.len() > 2 && normalized_target_relative.contains(*token))
                .count() as i32;
            score += matched_tokens * 3;
        }

        if bundle.id.starts_with("steam.") {
            let appid = bundle.id.trim_start_matches("steam.");
            if target_relative.contains(appid) {
                score += 18;
            }
            if target_relative.contains("compatdata") || target_relative.contains("userdata") {
                score += 4;
            }
        }

        if target_relative.contains("heroic")
            && bundle_tokens
                .iter()
                .any(|token| normalized_target_relative.contains(token))
        {
            score += 8;
        }
        if target_relative.contains("lutris")
            && bundle_tokens
                .iter()
                .any(|token| normalized_target_relative.contains(token))
        {
            score += 8;
        }
        if target_relative.contains("flatpak") || target_relative.contains(".var/app") {
            score += 4;
        }
        if target_relative.contains("wine") || target_relative.contains("proton") {
            score += 4;
        }

        for folder in &bundle.folder_bundles {
            let folder_target = folder.target.trim_end_matches('/');
            let folder_base = std::path::Path::new(folder_target)
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or(folder_target);
            let normalized_folder_base = normalize_name(folder_base);
            if normalized_folder_base == normalized_target_base {
                score += 10;
            }
            if !normalized_folder_base.is_empty()
                && normalized_target_relative.contains(&normalized_folder_base)
            {
                score += 6;
            }
            let folder_relative = folder_target
                .strip_prefix(home)
                .unwrap_or(folder_target)
                .trim_start_matches('/');
            if !folder_relative.is_empty() && target_relative.starts_with(folder_relative) {
                score += 6;
            }
            if target_relative.starts_with(".steam/steam/steamapps/compatdata/")
                && bundle.id.starts_with("steam.")
                && target_relative.contains(bundle.id.trim_start_matches("steam."))
            {
                score += 20;
            }
        }

        if score >= 5 {
            match &best_match {
                Some((_, best_score)) if score <= *best_score => {}
                _ => best_match = Some((bundle.id.clone(), score)),
            }
        }
    }

    best_match.map(|(bundle_id, _)| bundle_id)
}

fn name_tokens(value: &str) -> Vec<String> {
    value
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|part| part.len() > 2)
        .map(normalize_name)
        .filter(|part| !part.is_empty())
        .collect()
}

fn infer_auto_attachment_kind(target: &str) -> String {
    let normalized = target.to_ascii_lowercase();
    if normalized.contains("compatdata")
        || normalized.contains("proton")
        || normalized.contains(".wine")
        || normalized.contains("prefix")
    {
        return "runtime-prefix".to_string();
    }
    if normalized.contains("mods")
        || normalized.contains("workshop")
        || normalized.contains("plugins")
    {
        return "mods".to_string();
    }
    if normalized.contains("config") {
        return "config".to_string();
    }
    if normalized.contains("save")
        || normalized.contains("userdata")
        || normalized.contains("documents")
    {
        return "data".to_string();
    }
    "folder".to_string()
}

fn normalize_name(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn derive_learning_attachment_root(path: &str, home: &str) -> Option<String> {
    let normalized = path.trim_end_matches('/');
    if !normalized.starts_with(home) {
        return None;
    }
    let relative = normalized
        .strip_prefix(home)
        .unwrap_or(normalized)
        .trim_start_matches('/');
    if relative.is_empty() {
        return None;
    }
    let parts: Vec<&str> = relative
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    if parts.is_empty() {
        return None;
    }

    let root = match parts.as_slice() {
        [first, second, third, fourth, ..]
            if *first == ".local"
                && *second == "share"
                && *third == "Steam"
                && *fourth == "userdata" =>
        {
            if parts.len() >= 6 {
                format!(
                    "{home}/.local/share/Steam/userdata/{}/{}/",
                    parts[4], parts[5]
                )
                .trim_end_matches('/')
                .to_string()
            } else {
                format!("{home}/.local/share/Steam/userdata")
            }
        }
        [first, second, third, fourth, ..]
            if *first == ".steam"
                && *second == "steam"
                && *third == "steamapps"
                && *fourth == "compatdata" =>
        {
            if parts.len() >= 5 {
                format!("{home}/.steam/steam/steamapps/compatdata/{}", parts[4])
            } else {
                format!("{home}/.steam/steam/steamapps/compatdata")
            }
        }
        [first, second, third, fourth, ..]
            if *first == "Games" && *second == "Heroic" && *third == "Prefixes" =>
        {
            format!("{home}/Games/Heroic/Prefixes/{fourth}")
        }
        [first, second, third, fourth, ..]
            if *first == ".local" && *second == "share" && *third == "lutris" =>
        {
            format!("{home}/.local/share/lutris/{fourth}")
        }
        [first, second, third, ..] if *first == ".local" && *second == "share" => {
            format!("{home}/.local/share/{third}")
        }
        [first, second, third, ..] if *first == ".local" && *second == "state" => {
            format!("{home}/.local/state/{third}")
        }
        [first, second, third, ..] if *first == ".var" && *second == "app" => {
            format!("{home}/.var/app/{third}")
        }
        [first, second, third, ..] if *first == ".config" && *second == "heroic" => {
            format!("{home}/.config/heroic/{third}")
        }
        [first, second, third, ..] if *first == ".config" && *second == "lutris" => {
            format!("{home}/.config/lutris/{third}")
        }
        [first, second, ..]
            if matches!(
                *first,
                ".config"
                    | ".cache"
                    | "Documents"
                    | "Games"
                    | "Applications"
                    | "Downloads"
                    | ".steam"
                    | ".wine"
            ) =>
        {
            format!("{home}/{first}/{second}")
        }
        [first, second, ..] => format!("{home}/{first}/{second}"),
        [first] => format!("{home}/{first}"),
        _ => return None,
    };

    Some(root.trim_end_matches('/').to_string())
}

fn normalize_manual_attachment_path(
    path: &str,
    target_user: &str,
) -> Result<String, FrontendError> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(AppError::InvalidInput("Attachment path cannot be empty.".to_string()).into());
    }
    if !trimmed.starts_with('/') {
        return Err(AppError::InvalidInput("Attachment path must be absolute.".to_string()).into());
    }
    if trimmed.contains("..") {
        return Err(
            AppError::InvalidInput("Attachment path cannot contain '..'.".to_string()).into(),
        );
    }
    let allowed_prefix = format!("/home/{target_user}");
    if !trimmed.starts_with(&allowed_prefix) {
        return Err(AppError::InvalidInput(format!(
            "Attachment path must stay within {}.",
            allowed_prefix
        ))
        .into());
    }
    Ok(trimmed.trim_end_matches('/').to_string())
}

fn manual_attachment_id(path: &str) -> String {
    let sanitized: String = path
        .trim_start_matches('/')
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' => ch,
            _ => '-',
        })
        .collect();
    format!("manual-{}", sanitized)
}

fn manual_attachment_label(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or(path)
        .to_string()
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
