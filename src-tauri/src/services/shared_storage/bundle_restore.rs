use std::collections::HashMap;
use std::time::Duration;

use tokio::sync::RwLock;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::errors::{AppError, AppResult};
use crate::models::app_state::{
    AppBundle, BundleIndex, FolderBundle, RestoreDryRunItem, RestoreDryRunResult,
    RestoreJob, RestoreJobItem, RestoreRequest,
};

use crate::services::{app_context::AppContext, remote_exec::RemoteExec};

/// In-memory restore job tracking.
static RESTORE_JOBS: std::sync::OnceLock<RwLock<HashMap<String, RestoreJob>>> =
    std::sync::OnceLock::new();

fn get_restore_jobs() -> &'static RwLock<HashMap<String, RestoreJob>> {
    RESTORE_JOBS.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Bundle Restore service.
pub struct BundleRestoreService;

impl BundleRestoreService {
    /// List bundles for an instance by reading the remote bundle index.
    pub async fn list_bundles(
        context: &AppContext,
        remote: &RemoteExec,
        instance_id: u64,
        target_user: &str,
    ) -> AppResult<BundleIndex> {
        super::bundle_indexer::BundleIndexer::read_from_remote(
            context, remote, instance_id, target_user,
        )
        .await
    }

    /// Perform a dry-run restore and return what would be restored.
    pub async fn dry_run_restore(
        context: &AppContext,
        remote: &RemoteExec,
        instance_id: u64,
        target_user: &str,
        request: RestoreRequest,
    ) -> AppResult<RestoreDryRunResult> {
        let index = Self::list_bundles(context, remote, instance_id, target_user).await?;
        let bundle = Self::find_bundle(&index, &request.bundle_id)?;
        let selected = Self::resolve_selected_folders(bundle, &request.folder_bundle_ids)?;

        let mut items = Vec::new();
        for folder in &selected {
            items.push(RestoreDryRunItem {
                folder_bundle_id: folder.id.clone(),
                label: folder.label.clone(),
                source: folder.source.clone(),
                target: Self::compute_target(folder, &request.mode, &index.host.home)?,
                kind: folder.kind.clone(),
                action: "copy".to_string(),
            });
        }

        // Try to get a file count estimate via rclone --dry-run
        let estimate = Self::estimate_file_count(context, remote, target_user, &items).await?;

        Ok(RestoreDryRunResult {
            would_restore: items,
            total_files_estimate: estimate,
        })
    }

    /// Perform an actual restore.
    pub async fn restore_bundle(
        context: &AppContext,
        remote: &RemoteExec,
        instance_id: u64,
        target_user: &str,
        request: RestoreRequest,
    ) -> AppResult<RestoreJob> {
        let index = Self::list_bundles(context, remote, instance_id, target_user).await?;
        let bundle = Self::find_bundle(&index, &request.bundle_id)?;
        let selected = Self::resolve_selected_folders(bundle, &request.folder_bundle_ids)?;

        let job_id = Uuid::new_v4().to_string();
        let started_at = chrono::Local::now().to_rfc3339();

        let mut job_items = Vec::new();
        for folder in &selected {
            job_items.push(RestoreJobItem {
                folder_bundle_id: folder.id.clone(),
                label: folder.label.clone(),
                source: folder.source.clone(),
                target: Self::compute_target(folder, &request.mode, &index.host.home)?,
                kind: folder.kind.clone(),
                status: "pending".to_string(),
                error: None,
            });
        }

        let mut job = RestoreJob {
            job_id: job_id.clone(),
            instance_id,
            bundle_id: request.bundle_id.clone(),
            mode: request.mode.clone(),
            status: "running".to_string(),
            started_at: started_at.clone(),
            finished_at: None,
            items: job_items.clone(),
            error: None,
        };

        {
            let mut jobs = get_restore_jobs().write().await;
            jobs.insert(job_id.clone(), job.clone());
        }

        // Execute restore items sequentially
        for idx in 0..job.items.len() {
            job.items[idx].status = "running".to_string();
            {
                let mut jobs = get_restore_jobs().write().await;
                if let Some(j) = jobs.get_mut(&job_id) {
                    j.items[idx].status = "running".to_string();
                }
            }

            let item_ref = &job.items[idx];
            let result = Self::run_rclone_restore(context, remote, target_user, item_ref).await;

            match result {
                Ok(()) => {
                    job.items[idx].status = "completed".to_string();
                }
                Err(e) => {
                    let err = format!("{e}");
                    job.items[idx].status = "failed".to_string();
                    job.items[idx].error = Some(err.clone());
                    warn!(
                        job_id = %job_id,
                        folder = %job.items[idx].folder_bundle_id,
                        error = %err,
                        "Restore item failed"
                    );
                }
            }

            {
                let mut jobs = get_restore_jobs().write().await;
                if let Some(j) = jobs.get_mut(&job_id) {
                    j.items[idx] = job.items[idx].clone();
                }
            }
        }

        let all_failed = job.items.iter().all(|i| i.status == "failed");
        let any_failed = job.items.iter().any(|i| i.status == "failed");

        job.status = if all_failed {
            "failed".to_string()
        } else if any_failed {
            "partial".to_string()
        } else {
            "completed".to_string()
        };
        job.finished_at = Some(chrono::Local::now().to_rfc3339());

        {
            let mut jobs = get_restore_jobs().write().await;
            if let Some(j) = jobs.get_mut(&job_id) {
                *j = job.clone();
            }
        }

        info!(
            job_id = %job_id,
            status = %job.status,
            "Restore job finished"
        );

        Ok(job)
    }

    /// Get a restore job by ID.
    pub async fn get_job(job_id: &str) -> AppResult<RestoreJob> {
        let jobs = get_restore_jobs().read().await;
        jobs.get(job_id)
            .cloned()
            .ok_or_else(|| AppError::NotFound(format!("Restore job {} not found", job_id)))
    }

    // ------------------------------------------------------------------
    // Internal helpers
    // ------------------------------------------------------------------

    fn find_bundle<'a>(index: &'a BundleIndex, bundle_id: &str) -> AppResult<&'a AppBundle> {
        index
            .bundles
            .iter()
            .find(|b| b.id == bundle_id)
            .ok_or_else(|| AppError::NotFound(format!("Bundle {} not found", bundle_id)))
    }

    fn resolve_selected_folders(
        bundle: &AppBundle,
        folder_ids: &[String],
    ) -> AppResult<Vec<FolderBundle>> {
        if folder_ids.is_empty() {
            return Err(AppError::InvalidInput(
                "No folder bundles selected for restore".to_string(),
            ));
        }

        let mut selected = Vec::new();
        for id in folder_ids {
            let folder = bundle
                .folder_bundles
                .iter()
                .find(|f| f.id == *id)
                .ok_or_else(|| {
                    AppError::InvalidInput(format!(
                        "Folder bundle {} not found in bundle {}",
                        id, bundle.id
                    ))
                })?;
            selected.push(folder.clone());
        }
        Ok(selected)
    }

    fn compute_target(
        folder: &FolderBundle,
        mode: &str,
        home: &str,
    ) -> AppResult<String> {
        Self::validate_target(&folder.target, home)?;

        match mode {
            "merge" => Ok(folder.target.clone()),
            "restore_to_staging" => {
                let bundle_name = folder
                    .target
                    .split('/')
                    .last()
                    .unwrap_or("restored");
                let staging_base = format!("{}/Restored/{}", home, bundle_name);
                // Preserve relative structure under staging
                let relative = folder.source.strip_prefix("home/").unwrap_or(&folder.source);
                Ok(format!("{}/{}", staging_base, relative))
            }
            _ => Err(AppError::InvalidInput(format!("Unknown restore mode: {}", mode))),
        }
    }

    /// Validate that a target path is safe.
    fn validate_target(target: &str, home: &str) -> AppResult<()> {
        if !target.starts_with('/') {
            return Err(AppError::InvalidInput(format!(
                "Target path must be absolute: {}",
                target
            )));
        }
        if target.contains("..") {
            return Err(AppError::InvalidInput(format!(
                "Target path contains unsafe segment: {}",
                target
            )));
        }
        if !target.starts_with(home) {
            return Err(AppError::InvalidInput(format!(
                "Target path must be inside home directory ({}): {}",
                home, target
            )));
        }
        Ok(())
    }

    /// Validate that a source path is safe.
    fn validate_source(source: &str) -> AppResult<()> {
        if source.starts_with('/') {
            return Err(AppError::InvalidInput(format!(
                "Source path must be relative: {}",
                source
            )));
        }
        if source.contains("..") {
            return Err(AppError::InvalidInput(format!(
                "Source path contains unsafe segment: {}",
                source
            )));
        }
        Ok(())
    }

    async fn run_rclone_restore(
        context: &AppContext,
        remote: &RemoteExec,
        target_user: &str,
        item: &RestoreJobItem,
    ) -> AppResult<()> {
        Self::validate_source(&item.source)?;
        Self::validate_target(&item.target, &format!("/home/{}", target_user))?;

        let state = context.load_state().await;
        let settings = &state.shared_storage.settings;

        let effective_remote = if settings.crypt_password.as_deref().map(|s| !s.is_empty()).unwrap_or(false) {
            format!("{}-crypt", settings.remote_name)
        } else {
            settings.remote_name.clone()
        };
        let remote_root = format!(
            "{}:{}/{}",
            effective_remote, settings.bucket_name, settings.destination_prefix
        );

        let parent_dir = std::path::Path::new(&item.target)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| item.target.clone());

        // Ensure parent directory exists
        let mkdir_cmd = format!(
            "sudo -u {user} mkdir -p {dir}",
            user = target_user,
            dir = shell_escape(&parent_dir)
        );
        let mkdir_out = {
            let r = remote.clone();
            tokio::task::spawn_blocking(move || r.ssh(&mkdir_cmd, Duration::from_secs(30)))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };
        if mkdir_out.status_code != 0 {
            warn!(
                "mkdir for restore target failed: {}",
                mkdir_out.stderr.trim()
            );
        }

        let cmd = if item.kind == "file" {
            format!(
                "sudo -u {user} rclone copyto {src} {dst} --checksum --create-empty-src-dirs 2>&1",
                user = target_user,
                src = shell_escape(&format!("{}/{}", remote_root, item.source)),
                dst = shell_escape(&item.target),
            )
        } else {
            format!(
                "sudo -u {user} rclone copy {src} {dst} --checksum --create-empty-src-dirs --transfers=8 --checkers=16 2>&1",
                user = target_user,
                src = shell_escape(&format!("{}/{}", remote_root, item.source)),
                dst = shell_escape(&item.target),
            )
        };

        info!(
            "Running rclone restore: {}",
            cmd.replace(&settings.backblaze_application_key, "***")
                .replace(&settings.backblaze_key_id, "***")
        );

        let output = {
            let r = remote.clone();
            tokio::task::spawn_blocking(move || r.ssh(&cmd, Duration::from_secs(600)))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "rclone restore failed (exit {}): {}",
                output.status_code,
                output.stderr.trim()
            )));
        }

        Ok(())
    }

    async fn estimate_file_count(
        context: &AppContext,
        remote: &RemoteExec,
        target_user: &str,
        items: &[RestoreDryRunItem],
    ) -> AppResult<u32> {
        let state = context.load_state().await;
        let settings = &state.shared_storage.settings;
        let effective_remote = if settings.crypt_password.as_deref().map(|s| !s.is_empty()).unwrap_or(false) {
            format!("{}-crypt", settings.remote_name)
        } else {
            settings.remote_name.clone()
        };
        let remote_root = format!(
            "{}:{}/{}",
            effective_remote, settings.bucket_name, settings.destination_prefix
        );

        let mut total = 0u32;
        for item in items {
            let cmd = if item.kind == "file" {
                format!(
                    "sudo -u {user} rclone ls {src} --max-depth 0 --dry-run 2>&1 | wc -l",
                    user = target_user,
                    src = shell_escape(&format!("{}/{}", remote_root, item.source)),
                )
            } else {
                format!(
                    "sudo -u {user} rclone ls {src} --max-depth 100 --dry-run 2>&1 | wc -l",
                    user = target_user,
                    src = shell_escape(&format!("{}/{}", remote_root, item.source)),
                )
            };

            let output = {
                let r = remote.clone();
                tokio::task::spawn_blocking(move || r.ssh(&cmd, Duration::from_secs(60)))
                    .await
                    .map_err(|e| AppError::Command(format!("join failure: {e}")))??
            };

            if let Ok(count) = output.stdout.trim().parse::<u32>() {
                total += count;
            }
        }

        Ok(total)
    }
}

fn shell_escape(input: &str) -> String {
    input.replace('\'', "'\"'\"'")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::app_state::{BundleHost, FolderBundle};

    #[test]
    fn test_validate_source_rejects_absolute() {
        assert!(BundleRestoreService::validate_source("/etc/passwd").is_err());
    }

    #[test]
    fn test_validate_source_rejects_dotdot() {
        assert!(BundleRestoreService::validate_source("home/user/../../etc/passwd").is_err());
    }

    #[test]
    fn test_validate_source_accepts_relative() {
        assert!(BundleRestoreService::validate_source("home/user/.config/discord").is_ok());
    }

    #[test]
    fn test_validate_target_rejects_non_absolute() {
        assert!(BundleRestoreService::validate_target("home/user/foo", "/home/user").is_err());
    }

    #[test]
    fn test_validate_target_rejects_dotdot() {
        assert!(BundleRestoreService::validate_target("/home/user/../../etc/passwd", "/home/user").is_err());
    }

    #[test]
    fn test_validate_target_rejects_outside_home() {
        assert!(BundleRestoreService::validate_target("/etc/passwd", "/home/user").is_err());
    }

    #[test]
    fn test_validate_target_accepts_inside_home() {
        assert!(BundleRestoreService::validate_target("/home/user/.config/discord", "/home/user").is_ok());
    }

    #[test]
    fn test_compute_target_merge() {
        let folder = FolderBundle {
            id: "settings".to_string(),
            label: "Settings".to_string(),
            source: "home/user/.config/discord".to_string(),
            target: "/home/user/.config/discord".to_string(),
            kind: "folder".to_string(),
            default_selected: true,
        };
        let result = BundleRestoreService::compute_target(&folder, "merge", "/home/user").unwrap();
        assert_eq!(result, "/home/user/.config/discord");
    }

    #[test]
    fn test_compute_target_staging() {
        let folder = FolderBundle {
            id: "settings".to_string(),
            label: "Settings".to_string(),
            source: "home/user/.config/discord".to_string(),
            target: "/home/user/.config/discord".to_string(),
            kind: "folder".to_string(),
            default_selected: true,
        };
        let result = BundleRestoreService::compute_target(&folder, "restore_to_staging", "/home/user").unwrap();
        assert!(result.contains("/Restored/"));
        assert!(result.contains("user/.config/discord"));
    }

    #[test]
    fn test_find_bundle_found() {
        let index = BundleIndex {
            schema_version: 1,
            generated_at: "2026-04-24T20:00:00Z".to_string(),
            instance_id: 1,
            snapshot_id: "latest".to_string(),
            host: BundleHost { username: "user".to_string(), home: "/home/user".to_string(), os: "ubuntu".to_string() },
            bundles: vec![
                crate::models::app_state::AppBundle {
                    id: "app.discord".to_string(),
                    name: "Discord".to_string(),
                    bundle_type: "app".to_string(),
                    confidence: 0.92,
                    signals: vec!["desktop_launcher".to_string()],
                    folder_bundles: vec![],
                }
            ],
        };
        let bundle = BundleRestoreService::find_bundle(&index, "app.discord").unwrap();
        assert_eq!(bundle.name, "Discord");
    }

    #[test]
    fn test_find_bundle_not_found() {
        let index = BundleIndex {
            schema_version: 1,
            generated_at: "2026-04-24T20:00:00Z".to_string(),
            instance_id: 1,
            snapshot_id: "latest".to_string(),
            host: BundleHost { username: "user".to_string(), home: "/home/user".to_string(), os: "ubuntu".to_string() },
            bundles: vec![],
        };
        assert!(BundleRestoreService::find_bundle(&index, "app.missing").is_err());
    }
}
