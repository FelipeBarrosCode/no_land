use std::{collections::HashMap, sync::OnceLock};

use tauri::{AppHandle, State};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::{
    errors::{AppError, FrontendError},
    models::{
        app_state::PersistedAppState,
        launch_library::{LaunchLibraryResponse, LaunchSoftwareJob, SoftwareArtworkResult},
    },
    moonlight::composition::MoonlightManager,
    services::{
        app_config::IgdbConfig,
        app_context::AppContext,
        launch_library::{launch_remote_software, load_launch_library, LaunchLibraryEntry},
        shared_storage::shared_storage_manager::SharedStorageManager,
        software_artwork::SoftwareArtworkService,
    },
};

static LAUNCH_SOFTWARE_JOBS: OnceLock<RwLock<HashMap<String, LaunchSoftwareJob>>> = OnceLock::new();

fn launch_jobs() -> &'static RwLock<HashMap<String, LaunchSoftwareJob>> {
    LAUNCH_SOFTWARE_JOBS.get_or_init(|| RwLock::new(HashMap::new()))
}

#[tauri::command]
pub async fn get_instance_launch_library(
    instance_id: u64,
    context: State<'_, AppContext>,
) -> Result<LaunchLibraryResponse, FrontendError> {
    let remote = super::build_remote_exec_for_instance(context.inner(), instance_id).await?;
    let target_user = context.config.audio_target_user.clone();
    let launch_pc_available = launch_pc_available(context.inner(), instance_id).await;
    let (response, _) = load_launch_library(
        context.inner(),
        &remote,
        &target_user,
        instance_id,
        launch_pc_available,
    )
    .await?;
    Ok(response)
}

#[tauri::command]
pub async fn launch_instance_software(
    app: AppHandle,
    instance_id: u64,
    app_id: String,
    context: State<'_, AppContext>,
    moonlight: State<'_, MoonlightManager>,
) -> Result<LaunchSoftwareJob, FrontendError> {
    let app_id = app_id.trim().to_string();
    let mut job = LaunchSoftwareJob {
        job_id: Uuid::new_v4().to_string(),
        instance_id,
        app_id: app_id.clone(),
        status: "running".to_string(),
        restore_performed: false,
        stream_started: false,
        message: "Preparing software launch".to_string(),
        error: None,
        started_at: chrono::Utc::now().to_rfc3339(),
        finished_at: None,
    };
    launch_jobs()
        .write()
        .await
        .insert(job.job_id.clone(), job.clone());

    let result = run_launch(
        &app,
        instance_id,
        &app_id,
        context.inner(),
        moonlight.inner(),
        &mut job,
    )
    .await;

    job.finished_at = Some(chrono::Utc::now().to_rfc3339());
    match result {
        Ok(()) => {
            job.status = "completed".to_string();
            job.message = "Software launch requested successfully".to_string();
            job.error = None;
        }
        Err(error) => {
            job.status = "failed".to_string();
            job.message = "Software could not be launched".to_string();
            job.error = Some(error);
        }
    }
    launch_jobs()
        .write()
        .await
        .insert(job.job_id.clone(), job.clone());
    Ok(job)
}

#[tauri::command]
pub async fn get_launch_instance_software_job(
    job_id: String,
) -> Result<LaunchSoftwareJob, FrontendError> {
    launch_jobs()
        .read()
        .await
        .get(job_id.trim())
        .cloned()
        .ok_or_else(|| {
            AppError::NotFound(format!("Software launch job {} was not found", job_id)).into()
        })
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IgdbCredentialsUpdate {
    pub twitch_client_id: String,
    pub twitch_client_secret: String,
}

#[tauri::command]
pub async fn get_software_artwork(
    name: String,
    artwork: State<'_, SoftwareArtworkService>,
) -> Result<SoftwareArtworkResult, FrontendError> {
    Ok(artwork.get(&name).await)
}

#[tauri::command]
pub async fn update_igdb_credentials(
    payload: IgdbCredentialsUpdate,
    context: State<'_, AppContext>,
    artwork: State<'_, SoftwareArtworkService>,
) -> Result<PersistedAppState, FrontendError> {
    let twitch_client_id = payload.twitch_client_id.trim().to_string();
    let twitch_client_secret = payload.twitch_client_secret.trim().to_string();
    if twitch_client_id.is_empty() != twitch_client_secret.is_empty() {
        return Err(AppError::InvalidInput(
            "Enter both Twitch Client ID and Twitch Client Secret, or leave both empty."
                .to_string(),
        )
        .into());
    }

    let next_state = context
        .update_state(|state| {
            state.credentials.twitch_client_id = twitch_client_id.clone();
            state.credentials.twitch_client_secret = twitch_client_secret.clone();
            state.last_error = None;
        })
        .await?;

    artwork
        .update_config(resolve_igdb_config(context.inner(), &next_state))
        .await;

    Ok(next_state)
}

async fn run_launch(
    app: &AppHandle,
    instance_id: u64,
    app_id: &str,
    context: &AppContext,
    moonlight: &MoonlightManager,
    job: &mut LaunchSoftwareJob,
) -> Result<(), String> {
    if app_id.is_empty() {
        return Err("Choose an application to launch.".to_string());
    }
    let remote = super::build_remote_exec_for_instance(context, instance_id)
        .await
        .map_err(|error| error.to_string())?;
    let target_user = context.config.audio_target_user.clone();
    let (_, entries) = load_launch_library(
        context,
        &remote,
        &target_user,
        instance_id,
        launch_pc_available(context, instance_id).await,
    )
    .await
    .map_err(|error| error.to_string())?;
    let mut entry = find_entry(entries, app_id)?;

    if !entry.item.installed {
        let bundle_id = entry.item.latest_bundle_id.clone().ok_or_else(|| {
            format!(
                "{} is only in shared storage, but it has no restorable bundle.",
                entry.item.display_name
            )
        })?;
        SharedStorageManager::start_agent_restore(
            context,
            &remote,
            &target_user,
            app_id,
            &bundle_id,
            "complete_application",
        )
        .await
        .map_err(|error| format!("Could not restore {}: {error}", entry.item.display_name))?;
        job.restore_performed = true;
        persist_running_job(job).await;

        let (_, refreshed) = load_launch_library(
            context,
            &remote,
            &target_user,
            instance_id,
            launch_pc_available(context, instance_id).await,
        )
        .await
        .map_err(|error| error.to_string())?;
        if let Some(refreshed_entry) = refreshed
            .into_iter()
            .find(|candidate| candidate.item.app_id == app_id)
        {
            entry = refreshed_entry;
        }
    }

    if !entry.item.launchable {
        return Err(format!(
            "{} has no supported launch metadata. Open Launch PC and start it from the desktop, or reinstall it so Steam/a desktop entry/an executable can be discovered.",
            entry.item.display_name
        ));
    }

    super::start_launch_pc_for_instance(app, instance_id, context, moonlight)
        .await
        .map_err(frontend_error_message)?;
    job.stream_started = true;
    persist_running_job(job).await;

    launch_remote_software(&remote, &target_user, &entry)
        .await
        .map_err(|error| error.to_string())
}

fn find_entry(
    entries: Vec<LaunchLibraryEntry>,
    app_id: &str,
) -> Result<LaunchLibraryEntry, String> {
    entries
        .into_iter()
        .find(|entry| entry.item.app_id == app_id)
        .ok_or_else(|| {
            format!(
                "Application {app_id} was not found on this instance or in connected shared storage. Refresh the library and try again."
            )
        })
}

async fn persist_running_job(job: &LaunchSoftwareJob) {
    launch_jobs()
        .write()
        .await
        .insert(job.job_id.clone(), job.clone());
}

async fn launch_pc_available(context: &AppContext, instance_id: u64) -> bool {
    let state = context.load_state().await;
    state
        .provisioned_servers
        .iter()
        .find(|server| server.instance_id == instance_id)
        .is_some_and(|server| {
            !server.ssh_host.trim().is_empty()
                && server.ssh_port != 0
                && super::resolve_embedded_moonlight_host_address(&state, instance_id).is_some()
        })
}

fn frontend_error_message(error: FrontendError) -> String {
    match error.details {
        Some(details) if !details.trim().is_empty() => format!("{}: {}", error.message, details),
        _ => error.message,
    }
}

fn resolve_igdb_config(context: &AppContext, state: &PersistedAppState) -> IgdbConfig {
    let twitch_client_id = if state.credentials.twitch_client_id.trim().is_empty() {
        context.config.igdb.twitch_client_id.clone()
    } else {
        Some(state.credentials.twitch_client_id.trim().to_string())
    };
    let twitch_client_secret = if state.credentials.twitch_client_secret.trim().is_empty() {
        context.config.igdb.twitch_client_secret.clone()
    } else {
        Some(state.credentials.twitch_client_secret.trim().to_string())
    };
    IgdbConfig {
        twitch_client_id,
        twitch_client_secret,
    }
}
