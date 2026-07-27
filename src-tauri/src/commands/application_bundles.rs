use tauri::State;
use tracing::info;

use crate::errors::{AppError, FrontendError};
use crate::models::application_bundle::{
    ApplicationBundle, ApplicationBundleId, ApplicationCatalog, ApplicationCatalogEntry,
    ApplicationKind, BackupStatus, BundleAvailability, BundleComponent, BundleComponentId,
    BundleComponentKind, BundleRelationship, BundleRelationshipKind, BundleRestoreResult,
    BundleSnapshot, ComponentSummary, RestoreMode, RestorePlanRequest, SnapshotId,
};
use crate::services::app_context::AppContext;

// ─── Bundle Catalog Commands ──────────────────────────────

#[tauri::command]
pub async fn list_application_bundles(
    context: State<'_, AppContext>,
    _instance_id: Option<u64>,
) -> Result<Vec<ApplicationBundle>, FrontendError> {
    let state = context.load_state().await;

    // Return in-memory catalog if available, otherwise return demo data
    if !state.application_catalog.is_empty() {
        return Ok(state.application_catalog.clone());
    }

    // Return demo bundles for UI development
    Ok(build_demo_bundles())
}

#[tauri::command]
pub async fn get_application_bundle(
    context: State<'_, AppContext>,
    bundle_id: String,
) -> Result<ApplicationBundle, FrontendError> {
    let state = context.load_state().await;

    state
        .application_catalog
        .iter()
        .find(|b| b.id.0 == bundle_id)
        .cloned()
        .ok_or_else(|| AppError::NotFound(format!("Bundle {bundle_id} not found")).into())
}

#[tauri::command]
pub async fn list_bundle_snapshots(
    bundle_id: String,
) -> Result<Vec<BundleSnapshot>, FrontendError> {
    // Return demo snapshots
    Ok(build_demo_snapshots(&bundle_id))
}

#[tauri::command]
pub async fn discover_application_bundles(
    context: State<'_, AppContext>,
    instance_id: u64,
) -> Result<Vec<ApplicationBundle>, FrontendError> {
    info!("Discovering application bundles for instance {instance_id}");

    let bundles = build_demo_bundles();

    // Persist to in-memory catalog
    context
        .update_state(|state| {
            state.application_catalog = bundles.clone();
        })
        .await?;

    Ok(bundles)
}

// ─── Bundle Restore Commands ──────────────────────────────

#[tauri::command]
pub async fn restore_application_bundle(
    _context: State<'_, AppContext>,
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

    let plan = RestorePlanRequest {
        bundle_id: ApplicationBundleId(bundle_id.clone()),
        snapshot_id: snapshot_id.clone(),
        restore_mode: mode,
        selected_components,
    };

    info!("Restoring bundle {bundle_id} with mode {restore_mode}");

    let job_id = uuid::Uuid::new_v4().to_string();

    Ok(BundleRestoreResult {
        job_id: job_id.clone(),
        bundle_id: plan.bundle_id,
        snapshot_id: snapshot_id.unwrap_or_else(|| "latest".to_string()),
        status: "started".to_string(),
        total_components: 5,
        completed_components: 0,
        total_files: 0,
        restored_files: 0,
        total_bytes: 0,
        restored_bytes: 0,
        started_at: chrono::Utc::now().to_rfc3339(),
        finished_at: None,
        error: None,
    })
}

#[tauri::command]
pub async fn get_restore_job_status(job_id: String) -> Result<BundleRestoreResult, FrontendError> {
    info!("Checking restore job status: {job_id}");

    Ok(BundleRestoreResult {
        job_id: job_id.clone(),
        bundle_id: ApplicationBundleId("steam:1091500".to_string()),
        snapshot_id: "latest".to_string(),
        status: "in_progress".to_string(),
        total_components: 5,
        completed_components: 3,
        total_files: 12500,
        restored_files: 8200,
        total_bytes: 90_516_439_040,
        restored_bytes: 62_400_000_000,
        started_at: chrono::Utc::now().to_rfc3339(),
        finished_at: None,
        error: None,
    })
}

#[tauri::command]
pub async fn cancel_restore_job(job_id: String) -> Result<(), FrontendError> {
    info!("Cancelling restore job: {job_id}");
    Ok(())
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
    // In production, this creates a delayed garbage-collection marker
    Ok(())
}

#[tauri::command]
pub async fn trigger_bundle_backup(bundle_id: String) -> Result<(), FrontendError> {
    info!("Triggering backup for bundle: {bundle_id}");
    Ok(())
}

// ─── Catalog Commands ─────────────────────────────────────

#[tauri::command]
pub async fn get_application_catalog(
    context: State<'_, AppContext>,
) -> Result<ApplicationCatalog, FrontendError> {
    let state = context.load_state().await;

    let mut catalog = ApplicationCatalog {
        repository_id: "repo_demo".to_string(),
        generation: 1,
        generated_at: chrono::Utc::now().timestamp(),
        bundles: vec![],
    };

    for bundle in &state.application_catalog {
        catalog.bundles.push(build_catalog_entry(bundle));
    }

    Ok(catalog)
}

#[tauri::command]
pub async fn reload_application_catalog(
    context: State<'_, AppContext>,
) -> Result<ApplicationCatalog, FrontendError> {
    info!("Reloading application catalog");
    // In production, this downloads and decrypts the catalog from cloud storage
    get_application_catalog(context).await
}

// ─── Demo Data Builders ───────────────────────────────────

fn build_demo_bundles() -> Vec<ApplicationBundle> {
    let now = chrono::Utc::now().timestamp();
    vec![
        ApplicationBundle {
            id: ApplicationBundleId::new_steam("1091500"),
            display_name: "Cyberpunk 2077".to_string(),
            kind: ApplicationKind::SteamProton,
            store: Some(crate::models::application_bundle::ApplicationStore::Steam),
            store_application_id: Some("1091500".to_string()),
            store_metadata: Some(crate::models::application_bundle::StoreMetadata {
                store: crate::models::application_bundle::ApplicationStore::Steam,
                application_id: "1091500".to_string(),
                build_id: Some("13245678".to_string()),
                branch: Some("public".to_string()),
                installed_version: Some("2.3".to_string()),
            }),
            icon_object_key: None,
            components: build_cyberpunk_components(),
            relationships: vec![BundleRelationship {
                bundle_id: ApplicationBundleId::new_custom("redmod"),
                relationship_kind: BundleRelationshipKind::ModManager,
                display_name: "REDmod".to_string(),
            }],
            availability: BundleAvailability::Installed,
            backup_status: BackupStatus::Committed,
            latest_snapshot_id: Some("snap_001".to_string()),
            created_at: now - 86400 * 30,
            updated_at: now,
        },
        ApplicationBundle {
            id: ApplicationBundleId::new_native("blender"),
            display_name: "Blender".to_string(),
            kind: ApplicationKind::NativeLinux,
            store: None,
            store_application_id: None,
            store_metadata: None,
            icon_object_key: None,
            components: vec![
                BundleComponent {
                    id: BundleComponentId("blender-content".to_string()),
                    bundle_id: ApplicationBundleId::new_native("blender"),
                    kind: BundleComponentKind::ApplicationContent,
                    display_name: "Blender Installation".to_string(),
                    required: true,
                    selected_by_default: true,
                    restore_order: 1,
                    logical_size: 1_800_000_000,
                    file_count: 5200,
                    paths: vec![],
                },
                BundleComponent {
                    id: BundleComponentId("blender-config".to_string()),
                    bundle_id: ApplicationBundleId::new_native("blender"),
                    kind: BundleComponentKind::UserConfiguration,
                    display_name: "Blender Preferences".to_string(),
                    required: false,
                    selected_by_default: true,
                    restore_order: 2,
                    logical_size: 12_000_000,
                    file_count: 85,
                    paths: vec![],
                },
                BundleComponent {
                    id: BundleComponentId("blender-projects".to_string()),
                    bundle_id: ApplicationBundleId::new_native("blender"),
                    kind: BundleComponentKind::UserDocuments,
                    display_name: "Recent Projects".to_string(),
                    required: false,
                    selected_by_default: true,
                    restore_order: 3,
                    logical_size: 2_400_000_000,
                    file_count: 145,
                    paths: vec![],
                },
                BundleComponent {
                    id: BundleComponentId("blender-plugins".to_string()),
                    bundle_id: ApplicationBundleId::new_native("blender"),
                    kind: BundleComponentKind::Plugins,
                    display_name: "Add-ons".to_string(),
                    required: false,
                    selected_by_default: true,
                    restore_order: 4,
                    logical_size: 350_000_000,
                    file_count: 220,
                    paths: vec![],
                },
            ],
            relationships: vec![],
            availability: BundleAvailability::Installed,
            backup_status: BackupStatus::Committed,
            latest_snapshot_id: Some("snap_002".to_string()),
            created_at: now - 86400 * 14,
            updated_at: now - 3600,
        },
        ApplicationBundle {
            id: ApplicationBundleId::new_steam("1774580"),
            display_name: "Jedi Survivor".to_string(),
            kind: ApplicationKind::SteamProton,
            store: Some(crate::models::application_bundle::ApplicationStore::Steam),
            store_application_id: Some("1774580".to_string()),
            store_metadata: Some(crate::models::application_bundle::StoreMetadata {
                store: crate::models::application_bundle::ApplicationStore::Steam,
                application_id: "1774580".to_string(),
                build_id: Some("87654321".to_string()),
                branch: Some("public".to_string()),
                installed_version: Some("1.8".to_string()),
            }),
            icon_object_key: None,
            components: vec![
                BundleComponent {
                    id: BundleComponentId("jedi-content".to_string()),
                    bundle_id: ApplicationBundleId::new_steam("1774580"),
                    kind: BundleComponentKind::ApplicationContent,
                    display_name: "Game Installation".to_string(),
                    required: true,
                    selected_by_default: true,
                    restore_order: 1,
                    logical_size: 128_000_000_000,
                    file_count: 18000,
                    paths: vec![],
                },
                BundleComponent {
                    id: BundleComponentId("jedi-saves".to_string()),
                    bundle_id: ApplicationBundleId::new_steam("1774580"),
                    kind: BundleComponentKind::SaveFiles,
                    display_name: "Save Files".to_string(),
                    required: true,
                    selected_by_default: true,
                    restore_order: 2,
                    logical_size: 45_000_000,
                    file_count: 12,
                    paths: vec![],
                },
                BundleComponent {
                    id: BundleComponentId("jedi-prefix".to_string()),
                    bundle_id: ApplicationBundleId::new_steam("1774580"),
                    kind: BundleComponentKind::RuntimePrefix,
                    display_name: "Proton Prefix".to_string(),
                    required: true,
                    selected_by_default: true,
                    restore_order: 3,
                    logical_size: 2_100_000_000,
                    file_count: 3400,
                    paths: vec![],
                },
            ],
            relationships: vec![],
            availability: BundleAvailability::CloudOnly,
            backup_status: BackupStatus::Committed,
            latest_snapshot_id: Some("snap_003".to_string()),
            created_at: now - 86400 * 60,
            updated_at: now - 86400 * 3,
        },
    ]
}

fn build_cyberpunk_components() -> Vec<BundleComponent> {
    let bundle_id = ApplicationBundleId::new_steam("1091500");
    vec![
        BundleComponent {
            id: BundleComponentId("cp-content".to_string()),
            bundle_id: bundle_id.clone(),
            kind: BundleComponentKind::ApplicationContent,
            display_name: "Game Installation".to_string(),
            required: true,
            selected_by_default: true,
            restore_order: 1,
            logical_size: 83_200_000_000,
            file_count: 14500,
            paths: vec![],
        },
        BundleComponent {
            id: BundleComponentId("cp-saves".to_string()),
            bundle_id: bundle_id.clone(),
            kind: BundleComponentKind::SaveFiles,
            display_name: "Save Files".to_string(),
            required: true,
            selected_by_default: true,
            restore_order: 2,
            logical_size: 82_000_000,
            file_count: 28,
            paths: vec![],
        },
        BundleComponent {
            id: BundleComponentId("cp-config".to_string()),
            bundle_id: bundle_id.clone(),
            kind: BundleComponentKind::UserConfiguration,
            display_name: "User Settings".to_string(),
            required: true,
            selected_by_default: true,
            restore_order: 3,
            logical_size: 4_000_000,
            file_count: 15,
            paths: vec![],
        },
        BundleComponent {
            id: BundleComponentId("cp-mods".to_string()),
            bundle_id: bundle_id.clone(),
            kind: BundleComponentKind::Mods,
            display_name: "Mods".to_string(),
            required: false,
            selected_by_default: true,
            restore_order: 4,
            logical_size: 6_700_000_000,
            file_count: 340,
            paths: vec![],
        },
        BundleComponent {
            id: BundleComponentId("cp-prefix".to_string()),
            bundle_id: bundle_id.clone(),
            kind: BundleComponentKind::RuntimePrefix,
            display_name: "Proton Environment".to_string(),
            required: true,
            selected_by_default: true,
            restore_order: 5,
            logical_size: 2_100_000_000,
            file_count: 2800,
            paths: vec![],
        },
        BundleComponent {
            id: BundleComponentId("cp-shadercache".to_string()),
            bundle_id: bundle_id.clone(),
            kind: BundleComponentKind::ShaderCache,
            display_name: "Shader Cache".to_string(),
            required: false,
            selected_by_default: false,
            restore_order: 6,
            logical_size: 1_800_000_000,
            file_count: 1200,
            paths: vec![],
        },
    ]
}

fn build_demo_snapshots(bundle_id: &str) -> Vec<BundleSnapshot> {
    let now = chrono::Utc::now().timestamp();
    let bundle_id = ApplicationBundleId(bundle_id.to_string());

    vec![
        BundleSnapshot {
            id: SnapshotId("snap_001".to_string()),
            bundle_id: bundle_id.clone(),
            parent_snapshot_id: None,
            state: "committed".to_string(),
            reason: "Scheduled backup".to_string(),
            total_logical_size: 90_516_439_040,
            new_physical_size: 2_400_000_000,
            created_at: now - 7200,
            committed_at: Some(now - 7100),
        },
        BundleSnapshot {
            id: SnapshotId("snap_000".to_string()),
            bundle_id: bundle_id.clone(),
            parent_snapshot_id: Some("snap_001".to_string()),
            state: "committed".to_string(),
            reason: "Application exited".to_string(),
            total_logical_size: 90_480_000_000,
            new_physical_size: 1_800_000_000,
            created_at: now - 86400,
            committed_at: Some(now - 86300),
        },
    ]
}

fn build_catalog_entry(bundle: &ApplicationBundle) -> ApplicationCatalogEntry {
    let total_size: u64 = bundle.components.iter().map(|c| c.logical_size).sum();
    let required_size: u64 = bundle
        .components
        .iter()
        .filter(|c| c.required)
        .map(|c| c.logical_size)
        .sum();

    ApplicationCatalogEntry {
        bundle_id: bundle.id.clone(),
        display_name: bundle.display_name.clone(),
        kind: bundle.kind.clone(),
        icon_object_key: bundle.icon_object_key.clone(),
        latest_snapshot_id: bundle.latest_snapshot_id.clone(),
        total_logical_size: total_size,
        required_restore_size: required_size,
        last_backed_up_at: bundle.updated_at,
        component_summary: ComponentSummary {
            has_application_content: bundle
                .components
                .iter()
                .any(|c| c.kind == BundleComponentKind::ApplicationContent),
            has_saves: bundle
                .components
                .iter()
                .any(|c| c.kind == BundleComponentKind::SaveFiles),
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
            has_configuration: bundle
                .components
                .iter()
                .any(|c| c.kind == BundleComponentKind::UserConfiguration),
            has_plugins: bundle
                .components
                .iter()
                .any(|c| c.kind == BundleComponentKind::Plugins),
            component_count: bundle.components.len() as u32,
        },
    }
}
