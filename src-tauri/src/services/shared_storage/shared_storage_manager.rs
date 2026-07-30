use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use tokio::sync::RwLock;
use tracing::{error, info, warn};

use crate::errors::{AppError, AppResult};
use crate::models::application_bundle::{
    SharedStorageProfile, SharedStorageStatus, StorageProvider,
};

use crate::services::{app_context::AppContext, remote_exec::RemoteExec};

use super::bundle_indexer::BundleIndexer;
use super::object_storage::StorageCredential;
use super::provider_profiles::shared_profile_manager;

/// In-memory tracking of running backups per instance to prevent overlap.
static RUNNING_BACKUPS: std::sync::OnceLock<RwLock<HashMap<u64, BackupJobInfo>>> =
    std::sync::OnceLock::new();
static LISTING_READY_CACHE: std::sync::OnceLock<RwLock<HashSet<String>>> =
    std::sync::OnceLock::new();

fn get_running_backups() -> &'static RwLock<HashMap<u64, BackupJobInfo>> {
    RUNNING_BACKUPS.get_or_init(|| RwLock::new(HashMap::new()))
}

fn get_listing_ready_cache() -> &'static RwLock<HashSet<String>> {
    LISTING_READY_CACHE.get_or_init(|| RwLock::new(HashSet::new()))
}

#[derive(Debug, Clone)]
struct BackupJobInfo;

#[derive(Debug, Clone)]
pub(crate) struct ActiveSharedStorageProfile {
    pub profile: SharedStorageProfile,
    pub credentials: StorageCredential,
    pub provider_fields: HashMap<String, String>,
    pub remote_name: String,
}

#[derive(Debug, Clone)]
pub(crate) struct SharedStorageNamespace {
    pub root: String,
    pub metadata_root: String,
    pub catalogs_root: String,
    pub healthcheck_root: String,
}

/// Shared Storage Manager service.
///
/// Handles Backblaze B2 backup configuration, manual/scheduled backup
/// triggering, and status tracking for provisioned VM instances.
pub struct SharedStorageManager;

impl SharedStorageManager {
    const SELECTED_ONLY_MESSAGE: &'static str =
        "Shared storage now only moves files you explicitly select in the interface.";
    const SCHEDULED_BACKUPS_DISABLED_MESSAGE: &'static str =
        "Scheduled shared-storage backups are disabled. Save selected files manually from the shared storage interface.";

    fn profile_not_connected_message() -> String {
        "No shared storage provider is connected. Connect a provider profile first.".to_string()
    }

    pub async fn list_local_objects(
        context: &AppContext,
        remote: &RemoteExec,
        target_user: &str,
    ) -> AppResult<Vec<crate::models::app_state::SharedStorageObjectEntry>> {
        let _active_profile = Self::resolve_active_profile(context).await?;

        Self::ensure_rclone_installed(remote).await?;
        Self::write_filter_rules(remote, target_user).await?;

        let filter_path = format!("/home/{}/rules.txt", target_user);
        let list_cmd = format!(
            "sudo rclone lsf / --files-only --recursive --filter-from {filter} --checksum",
            filter = shell_escape(&filter_path),
        );

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh_until_complete(&list_cmd))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed listing local files for export: {}",
                format!("{}\n{}", output.stdout.trim(), output.stderr.trim())
            )));
        }

        let mut entries = Vec::new();
        let mut directory_paths = HashSet::new();
        for line in output.stdout.lines() {
            let normalized = line.trim().trim_start_matches('/').trim_end_matches('/');
            if normalized.is_empty() {
                continue;
            }

            let path = format!("/{}", normalized);
            let (parent_path, name) = split_parent_and_name(&path);
            entries.push(crate::models::app_state::SharedStorageObjectEntry {
                path: path.to_string(),
                name,
                parent_path,
                is_dir: false,
            });

            let mut cursor = parent_dir(normalized);
            while !cursor.is_empty() && cursor != "/" {
                directory_paths.insert(cursor.clone());
                cursor = parent_dir(&cursor);
            }
        }

        for dir_path in directory_paths {
            let (parent_path, name) = split_parent_and_name(&dir_path);
            entries.push(crate::models::app_state::SharedStorageObjectEntry {
                path: dir_path,
                name,
                parent_path,
                is_dir: true,
            });
        }

        entries.sort_by(|a, b| a.path.cmp(&b.path));
        entries.dedup_by(|a, b| a.path == b.path && a.is_dir == b.is_dir);
        Ok(entries)
    }

    pub async fn backup_selected_paths(
        context: &AppContext,
        remote: &RemoteExec,
        instance_id: u64,
        target_user: &str,
        selected_paths: &[String],
    ) -> AppResult<String> {
        if selected_paths.is_empty() {
            return Err(AppError::InvalidInput(
                "Select at least one file or folder to export.".to_string(),
            ));
        }

        let active_profile = Self::resolve_active_profile(context).await?;

        Self::ensure_rclone_installed(remote).await?;
        Self::configure_rclone_remote_for_profile(remote, target_user, &active_profile).await?;

        let dest = Self::build_profile_storage_source(&active_profile);
        let rclone_config_path = format!("/home/{}/.config/rclone/rclone.conf", target_user);

        let mut include_lines = String::new();
        for path in selected_paths {
            let normalized = normalize_selection_path(path)?;
            include_lines.push_str(&format!("+ {normalized}\n"));
            include_lines.push_str(&format!("+ {normalized}/**\n"));
        }
        include_lines.push_str("- **\n");

        let filter_path = format!("/home/{}/noland-export-selection.filter", target_user);
        let write_filter_cmd = format!(
            r#"sudo -u {user} bash -lc 'cat > {path} <<'"'"'EOF'"'"'
{content}EOF
chmod 600 {path}'"#,
            user = target_user,
            path = filter_path,
            content = include_lines,
        );

        {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh(&write_filter_cmd, Duration::from_secs(60))
            })
            .await
            .map_err(|e| AppError::Command(format!("join failure: {e}")))??;
        }

        let backup_cmd = format!(
            "sudo rclone copy / {dest} --config {config} --filter-from {filter} --checksum",
            dest = shell_escape(&dest),
            config = shell_escape(&rclone_config_path),
            filter = shell_escape(&filter_path),
        );

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh_until_complete(&backup_cmd))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };

        if output.status_code != 0 {
            let combined = redact_profile_secrets(
                &format!("{}\n{}", output.stdout, output.stderr),
                &active_profile,
            );
            let actionable = extract_actionable_rclone_error(&combined)
                .unwrap_or_else(|| combined.trim().to_string());
            return Err(AppError::Provisioning(format!(
                "Selective export failed (exit {}): {}",
                output.status_code, actionable
            )));
        }

        info!(
            instance_id = instance_id,
            count = selected_paths.len(),
            "Selective shared storage export completed"
        );
        Ok(format!(
            "Exported {} selected items to shared storage",
            selected_paths.len()
        ))
    }

    pub async fn list_remote_objects(
        context: &AppContext,
        remote: &RemoteExec,
        target_user: &str,
    ) -> AppResult<Vec<crate::models::app_state::SharedStorageObjectEntry>> {
        info!(
            target_user = target_user,
            "shared-storage list_remote_objects start"
        );
        let total_started = Instant::now();
        let active_profile = Self::resolve_active_profile(context).await?;

        let cache_key = listing_cache_key(remote, target_user, &active_profile);
        let cached_ready = {
            let cache = get_listing_ready_cache().read().await;
            cache.contains(&cache_key)
        };

        if !cached_ready {
            let setup_started = Instant::now();
            Self::ensure_rclone_installed(remote).await?;
            Self::configure_rclone_remote_for_profile(remote, target_user, &active_profile).await?;
            {
                let mut cache = get_listing_ready_cache().write().await;
                cache.insert(cache_key.clone());
            }
            info!(
                elapsed_ms = setup_started.elapsed().as_millis() as u64,
                "shared-storage list setup complete"
            );
        } else {
            info!("shared-storage list setup cache hit");
        }

        let source = Self::build_profile_storage_source(&active_profile);
        info!(source = source, "shared-storage list source resolved");

        let list_cmd = format!(
            "sudo -u {user} rclone lsf {src} --recursive --files-only --fast-list",
            user = target_user,
            src = shell_escape(&source),
        );

        let listing_started = Instant::now();
        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh_until_complete(&list_cmd))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };

        info!(
            status_code = output.status_code,
            stdout_len = output.stdout.len(),
            stderr_len = output.stderr.len(),
            elapsed_ms = listing_started.elapsed().as_millis() as u64,
            "shared-storage list_remote_objects command finished"
        );

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed listing remote objects for {}: {}",
                active_profile.profile.provider.label(),
                redact_profile_secrets(
                    &format!("{}\n{}", output.stdout, output.stderr),
                    &active_profile
                )
                .trim()
            )));
        }

        let parsing_started = Instant::now();
        let mut entries = Vec::new();
        let mut directory_paths = HashSet::new();

        for raw_line in output.stdout.lines() {
            let line = raw_line.trim();
            if line.is_empty() {
                continue;
            }

            let is_dir = false;
            let normalized = line.trim_end_matches('/').trim();
            if normalized.is_empty() {
                continue;
            }

            let path = format!("/{}", normalized);
            let (parent_path, name) = split_parent_and_name(&path);

            entries.push(crate::models::app_state::SharedStorageObjectEntry {
                path,
                name,
                parent_path,
                is_dir,
            });

            let mut cursor = parent_dir(normalized);
            while !cursor.is_empty() && cursor != "/" {
                directory_paths.insert(cursor.clone());
                cursor = parent_dir(&cursor);
            }
        }

        for dir_path in directory_paths {
            let (parent_path, name) = split_parent_and_name(&dir_path);
            entries.push(crate::models::app_state::SharedStorageObjectEntry {
                path: dir_path,
                name,
                parent_path,
                is_dir: true,
            });
        }

        entries.sort_by(|a, b| a.path.cmp(&b.path));
        entries.dedup_by(|a, b| a.path == b.path && a.is_dir == b.is_dir);
        info!(
            count = entries.len(),
            parse_elapsed_ms = parsing_started.elapsed().as_millis() as u64,
            total_elapsed_ms = total_started.elapsed().as_millis() as u64,
            "shared-storage list_remote_objects complete"
        );
        Ok(entries)
    }

    pub async fn restore_selected_paths(
        context: &AppContext,
        remote: &RemoteExec,
        instance_id: u64,
        target_user: &str,
        selected_paths: &[String],
    ) -> AppResult<String> {
        info!(
            instance_id = instance_id,
            selected_count = selected_paths.len(),
            "shared-storage restore_selected_paths start"
        );
        if selected_paths.is_empty() {
            return Err(AppError::InvalidInput(
                "Select at least one file or folder to sync.".to_string(),
            ));
        }

        let active_profile = Self::resolve_active_profile(context).await?;

        Self::ensure_rclone_installed(remote).await?;
        Self::write_filter_rules(remote, target_user).await?;
        Self::configure_rclone_remote_for_profile(remote, target_user, &active_profile).await?;

        let source = Self::build_profile_storage_source(&active_profile);

        let mut include_lines = String::new();
        for path in selected_paths {
            let normalized = normalize_selection_path(path)?;
            info!(path = normalized, "shared-storage selected path");
            include_lines.push_str(&format!("+ {normalized}\n"));
            include_lines.push_str(&format!("+ {normalized}/**\n"));
        }
        include_lines.push_str("- **\n");

        let filter_path = format!("/home/{}/noland-sync-selection.filter", target_user);
        let write_filter_cmd = format!(
            r#"sudo -u {user} bash -lc 'cat > {path} <<'"'"'EOF'"'"'
{content}EOF
chmod 600 {path}'"#,
            user = target_user,
            path = filter_path,
            content = include_lines,
        );

        {
            let remote = remote.clone();
            let write = tokio::task::spawn_blocking(move || {
                remote.ssh(&write_filter_cmd, Duration::from_secs(60))
            })
            .await
            .map_err(|e| AppError::Command(format!("join failure: {e}")))??;
            info!(
                status_code = write.status_code,
                "shared-storage selection filter file written"
            );
        }

        let restore_cmd = format!(
            "sudo -u {user} rclone copy {src} / --filter-from {filter} --checksum --update 2>&1",
            user = target_user,
            src = shell_escape(&source),
            filter = shell_escape(&filter_path),
        );

        let restore = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh_until_complete(&restore_cmd))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };

        info!(
            status_code = restore.status_code,
            stdout_len = restore.stdout.len(),
            stderr_len = restore.stderr.len(),
            "shared-storage selective restore command finished"
        );

        if restore.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Selective sync failed (exit {}): {}",
                restore.status_code,
                redact_profile_secrets(&restore.stderr, &active_profile).trim()
            )));
        }

        info!(
            instance_id = instance_id,
            count = selected_paths.len(),
            "Selective shared storage sync completed"
        );
        Ok(format!(
            "Synced {} selected items from {}",
            selected_paths.len(),
            active_profile.profile.provider.label()
        ))
    }

    /// Save shared storage settings into persisted app state.
    pub async fn save_settings(
        context: &AppContext,
        payload: crate::models::app_state::SharedStorageSettingsUpdate,
    ) -> AppResult<()> {
        let mut settings = crate::models::app_state::SharedStorageSettings::default();
        settings.enabled = payload.enabled;
        settings.backblaze_key_id = payload.backblaze_key_id;
        settings.backblaze_application_key = payload.backblaze_application_key;
        settings.bucket_name = if payload.bucket_name.trim().is_empty() {
            "noland".to_string()
        } else {
            payload.bucket_name
        };
        settings.remote_name = if payload.remote_name.trim().is_empty() {
            "b2".to_string()
        } else {
            payload.remote_name
        };
        settings.destination_prefix = if payload.destination_prefix.trim().is_empty() {
            "vm-backup".to_string()
        } else {
            payload.destination_prefix
        };
        // Only update crypt_password if a new one is provided (non-empty)
        if let Some(ref pwd) = payload.crypt_password {
            if !pwd.trim().is_empty() {
                settings.crypt_password = Some(pwd.clone());
            }
        }

        context
            .update_state(|state| {
                state.shared_storage.settings = settings;
            })
            .await?;

        info!("Shared storage settings saved");
        Ok(())
    }

    /// Return non-secret settings for the frontend.
    pub async fn get_settings(
        context: &AppContext,
    ) -> AppResult<crate::models::app_state::SharedStorageSettingsResponse> {
        let state = context.load_state().await;
        let s = &state.shared_storage.settings;
        Ok(crate::models::app_state::SharedStorageSettingsResponse {
            enabled: s.enabled,
            backblaze_key_id: s.backblaze_key_id.clone(),
            bucket_name: s.bucket_name.clone(),
            remote_name: s.remote_name.clone(),
            destination_prefix: s.destination_prefix.clone(),
            crypt_password_set: s
                .crypt_password
                .as_deref()
                .map(|p| !p.is_empty())
                .unwrap_or(false),
        })
    }

    /// Test the Backblaze B2 configuration by creating the rclone remote
    /// and running a lightweight `rclone ls`.
    pub async fn test_configuration(
        context: &AppContext,
        remote: &RemoteExec,
        target_user: &str,
    ) -> AppResult<()> {
        let _ = Self::test_profile_connection(context, remote, target_user, None).await?;
        Ok(())
    }

    pub async fn test_profile_connection(
        context: &AppContext,
        remote: &RemoteExec,
        target_user: &str,
        profile_id: Option<&str>,
    ) -> AppResult<crate::models::application_bundle::SharedStorageTestResult> {
        let started = Instant::now();
        info!(
            event = "shared_storage_test_start",
            target_user = target_user,
            profile_id = profile_id.unwrap_or("<active>"),
            "Shared storage configuration test started"
        );

        let active_profile = Self::resolve_profile(context, profile_id).await?;

        Self::ensure_rclone_installed(remote).await?;
        Self::configure_rclone_remote_for_profile(remote, target_user, &active_profile).await?;

        let source = Self::build_profile_storage_source(&active_profile);
        let list_cmd = format!(
            "sudo -u {user} rclone lsf {src} --max-depth 1 2>&1 | sed -n '1,20p'",
            user = target_user,
            src = shell_escape(&source),
        );

        let list_output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&list_cmd, Duration::from_secs(60)))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };

        let test_stdout = redact_profile_secrets(&list_output.stdout, &active_profile);
        let test_stderr = redact_profile_secrets(&list_output.stderr, &active_profile);
        info!(
            event = "shared_storage_test_output",
            target_user = target_user,
            status_code = list_output.status_code,
            stdout = %test_stdout.trim(),
            stderr = %test_stderr.trim(),
            "Shared storage test command output"
        );

        if list_output.status_code != 0 {
            let combined = redact_profile_secrets(
                &format!("{}\n{}", list_output.stdout, list_output.stderr),
                &active_profile,
            );
            let actionable = extract_actionable_rclone_error(&combined)
                .unwrap_or_else(|| combined.trim().to_string());
            return Ok(crate::models::application_bundle::SharedStorageTestResult {
                authenticated: false,
                can_list: false,
                can_write: false,
                can_read: false,
                can_delete_test_object: false,
                repository_accessible: false,
                latency_ms: Some(started.elapsed().as_millis() as u64),
                error: Some(actionable),
            });
        }

        let marker = format!(
            "noland-shared-storage-test-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        let local_test_path = format!("/tmp/{}.txt", marker);
        let remote_test_path = format!("{}/.noland-healthcheck/{}.txt", source, marker);
        let write_local_cmd = format!(
            "sudo -u {user} bash -lc 'printf %s {marker} > {path} && chmod 600 {path}'",
            user = target_user,
            marker = shell_escape(&marker),
            path = shell_escape(&local_test_path),
        );
        {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh(&write_local_cmd, Duration::from_secs(30))
            })
            .await
            .map_err(|e| AppError::Command(format!("join failure: {e}")))??;
        }

        let upload_cmd = format!(
            "sudo -u {user} rclone copyto {local} {remote_path} 2>&1",
            user = target_user,
            local = shell_escape(&local_test_path),
            remote_path = shell_escape(&remote_test_path),
        );
        let upload_output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&upload_cmd, Duration::from_secs(60)))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };
        if upload_output.status_code != 0 {
            let combined = redact_profile_secrets(
                &format!("{}\n{}", upload_output.stdout, upload_output.stderr),
                &active_profile,
            );
            let actionable = extract_actionable_rclone_error(&combined)
                .unwrap_or_else(|| combined.trim().to_string());
            return Ok(crate::models::application_bundle::SharedStorageTestResult {
                authenticated: true,
                can_list: true,
                can_write: false,
                can_read: false,
                can_delete_test_object: false,
                repository_accessible: true,
                latency_ms: Some(started.elapsed().as_millis() as u64),
                error: Some(format!("Write test failed: {actionable}")),
            });
        }

        let read_cmd = format!(
            "sudo -u {user} rclone cat {remote_path} 2>&1",
            user = target_user,
            remote_path = shell_escape(&remote_test_path),
        );
        let read_output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&read_cmd, Duration::from_secs(60)))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };
        if read_output.status_code != 0 || !read_output.stdout.contains(&marker) {
            let combined = redact_profile_secrets(
                &format!("{}\n{}", read_output.stdout, read_output.stderr),
                &active_profile,
            );
            let actionable = extract_actionable_rclone_error(&combined)
                .unwrap_or_else(|| combined.trim().to_string());
            return Ok(crate::models::application_bundle::SharedStorageTestResult {
                authenticated: true,
                can_list: true,
                can_write: true,
                can_read: false,
                can_delete_test_object: false,
                repository_accessible: true,
                latency_ms: Some(started.elapsed().as_millis() as u64),
                error: Some(format!("Read test failed: {actionable}")),
            });
        }

        let delete_cmd = format!(
            "sudo -u {user} rclone deletefile {remote_path} 2>&1",
            user = target_user,
            remote_path = shell_escape(&remote_test_path),
        );
        let delete_output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&delete_cmd, Duration::from_secs(60)))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };
        if delete_output.status_code != 0 {
            let combined = redact_profile_secrets(
                &format!("{}\n{}", delete_output.stdout, delete_output.stderr),
                &active_profile,
            );
            let actionable = extract_actionable_rclone_error(&combined)
                .unwrap_or_else(|| combined.trim().to_string());
            return Ok(crate::models::application_bundle::SharedStorageTestResult {
                authenticated: true,
                can_list: true,
                can_write: true,
                can_read: true,
                can_delete_test_object: false,
                repository_accessible: true,
                latency_ms: Some(started.elapsed().as_millis() as u64),
                error: Some(format!("Delete test failed: {actionable}")),
            });
        }

        let cleanup_cmd = format!(
            "sudo rm -f {path} >/dev/null 2>&1 || true",
            path = shell_escape(&local_test_path),
        );
        let _ = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&cleanup_cmd, Duration::from_secs(15)))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };

        Ok(crate::models::application_bundle::SharedStorageTestResult {
            authenticated: true,
            can_list: true,
            can_write: true,
            can_read: true,
            can_delete_test_object: true,
            repository_accessible: true,
            latency_ms: Some(started.elapsed().as_millis() as u64),
            error: None,
        })
    }

    /// Legacy whole-instance backup entrypoint.
    ///
    /// The app now only supports explicit selected-path exports via
    /// `backup_selected_paths`, so this path is intentionally disabled.
    pub async fn trigger_manual_backup(
        _context: &AppContext,
        _remote: &RemoteExec,
        _instance_id: u64,
        _target_user: &str,
    ) -> AppResult<()> {
        Err(AppError::InvalidInput(
            "Whole-instance shared-storage backup has been removed. Export selected files or folders instead."
                .to_string(),
        ))
    }

    /// Internal backup trigger with concurrency guard.
    #[allow(dead_code)]
    async fn trigger_backup(
        context: &AppContext,
        remote: &RemoteExec,
        instance_id: u64,
        target_user: &str,
        trigger: &str,
    ) -> AppResult<()> {
        info!(
            event = "shared_storage_backup_start",
            instance_id = instance_id,
            target_user = target_user,
            trigger = trigger,
            "Shared storage backup started"
        );
        // Concurrency guard
        {
            let running = get_running_backups().read().await;
            if running.contains_key(&instance_id) {
                return Err(AppError::Provisioning(
                    "A backup is already running for this instance.".to_string(),
                ));
            }
        }

        let active_profile = Self::resolve_active_profile(context).await?;

        // Mark backup as running
        let started_at = chrono::Local::now().to_rfc3339();
        {
            let mut running = get_running_backups().write().await;
            running.insert(instance_id, BackupJobInfo);
        }

        // Update state to running
        context
            .update_state(|state| {
                state.shared_storage.last_backup_started_at = Some(started_at.clone());
                state.shared_storage.last_backup_status = "running".to_string();
                state.shared_storage.last_backup_trigger = trigger.to_string();
                state.shared_storage.last_backup_error = None;
            })
            .await?;

        let result = Self::run_backup(remote, target_user, &active_profile, trigger).await;

        // Mark backup as no longer running
        {
            let mut running = get_running_backups().write().await;
            running.remove(&instance_id);
        }

        let finished_at = chrono::Local::now().to_rfc3339();
        match result {
            Ok(()) => {
                // Keep restore choices fresh in UI after each successful backup.
                if let Err(error) =
                    BundleIndexer::generate_and_upload(context, remote, instance_id, target_user)
                        .await
                {
                    warn!(
                        instance_id = instance_id,
                        trigger = trigger,
                        error = %error,
                        "Backup succeeded but bundle indexing failed"
                    );
                }

                context
                    .update_state(|state| {
                        state.shared_storage.last_backup_finished_at = Some(finished_at);
                        state.shared_storage.last_backup_status = "success".to_string();
                        state.shared_storage.last_backup_error = None;
                    })
                    .await?;
                info!(
                    event = "shared_storage_backup_success",
                    instance_id = instance_id,
                    target_user = target_user,
                    trigger = trigger,
                    "Backup completed successfully"
                );
                Ok(())
            }
            Err(e) => {
                let err_msg = format!("{e}");
                context
                    .update_state(|state| {
                        state.shared_storage.last_backup_finished_at = Some(finished_at);
                        state.shared_storage.last_backup_status = "failed".to_string();
                        state.shared_storage.last_backup_error = Some(err_msg.clone());
                    })
                    .await?;
                error!(
                    event = "shared_storage_backup_failure",
                    instance_id = instance_id,
                    target_user = target_user,
                    trigger = trigger,
                    error = %err_msg,
                    "Backup failed"
                );
                Err(e)
            }
        }
    }

    /// Run the actual rclone sync backup.
    #[allow(dead_code)]
    async fn run_backup(
        remote: &RemoteExec,
        target_user: &str,
        active_profile: &ActiveSharedStorageProfile,
        trigger: &str,
    ) -> AppResult<()> {
        // Ensure rclone is installed
        Self::ensure_rclone_installed(remote).await?;

        // Write filter rules
        Self::write_filter_rules(remote, target_user).await?;

        // Configure rclone remote
        Self::configure_rclone_remote_for_profile(remote, target_user, active_profile).await?;

        let filter_path = format!("/home/{}/rules.txt", target_user);
        let rclone_config_path = format!("/home/{}/.config/rclone/rclone.conf", target_user);
        let dest = Self::build_profile_storage_source(active_profile);

        let progress_flag = if trigger == "manual" {
            " --progress"
        } else {
            ""
        };
        let cmd = format!(
            "sudo rclone copy / {dest} --config {config} --filter-from {filter} --checksum{progress}",
            dest = shell_escape(&dest),
            config = shell_escape(&rclone_config_path),
            filter = shell_escape(&filter_path),
            progress = progress_flag,
        );

        info!(
            trigger = trigger,
            destination = %dest,
            "Starting rclone sync backup"
        );

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh_until_complete(&cmd))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };

        let stdout = output.stdout.clone();
        let stderr = output.stderr.clone();

        if !stdout.trim().is_empty() {
            info!(
                trigger = trigger,
                "shared-storage backup stdout:\n{}",
                stdout.trim()
            );
        }
        if !stderr.trim().is_empty() {
            warn!(
                trigger = trigger,
                "shared-storage backup stderr:\n{}",
                stderr.trim()
            );
        }

        if output.status_code != 0 {
            let combined = format!("{}\n{}", stdout.trim(), stderr.trim());
            let actionable = extract_actionable_rclone_error(&combined)
                .unwrap_or_else(|| combined.trim().to_string());
            let combined = format!("{}\n{}", stdout, stderr).to_ascii_lowercase();
            if combined.contains("storage_cap_exceeded")
                || combined.contains("cannot upload files, storage cap exceeded")
            {
                return Err(AppError::Provisioning(
                    "Object storage quota/cap exceeded. Increase your provider quota or available space, then retry Save."
                        .to_string(),
                ));
            }
            return Err(AppError::Provisioning(format!(
                "rclone sync failed (exit {}): {}",
                output.status_code, actionable
            )));
        }

        info!("rclone sync completed successfully");
        Ok(())
    }

    pub(crate) async fn prepare_active_profile_remote(
        context: &AppContext,
        remote: &RemoteExec,
        target_user: &str,
    ) -> AppResult<ActiveSharedStorageProfile> {
        let active_profile = Self::resolve_active_profile(context).await?;
        Self::ensure_rclone_installed(remote).await?;
        Self::configure_rclone_remote_for_profile(remote, target_user, &active_profile).await?;
        Ok(active_profile)
    }

    pub(crate) fn namespace_for_profile(
        active_profile: &ActiveSharedStorageProfile,
    ) -> SharedStorageNamespace {
        let root = Self::build_profile_storage_source(active_profile);
        let metadata_root = format!("{}/metadata", root);
        let catalogs_root = format!("{}/catalogs", root);
        let healthcheck_root = format!("{}/.noland-healthcheck", root);
        SharedStorageNamespace {
            root,
            metadata_root,
            catalogs_root,
            healthcheck_root,
        }
    }

    pub(crate) async fn resolve_active_profile(
        context: &AppContext,
    ) -> AppResult<ActiveSharedStorageProfile> {
        Self::resolve_profile(context, None).await
    }

    pub(crate) async fn resolve_profile(
        context: &AppContext,
        profile_id: Option<&str>,
    ) -> AppResult<ActiveSharedStorageProfile> {
        let state = context.load_state().await;
        let reference = if let Some(profile_id) = profile_id {
            state
                .shared_storage_profiles
                .iter()
                .find(|profile| profile.id == profile_id)
                .cloned()
        } else {
            state
                .shared_storage_profiles
                .iter()
                .find(|profile| profile.active)
                .cloned()
                .or_else(|| state.shared_storage_profiles.first().cloned())
        }
        .ok_or_else(|| {
            AppError::InvalidInput(
                "No shared storage profile connected. Connect a provider first.".to_string(),
            )
        })?;

        let provider = reference
            .provider
            .clone()
            .or_else(|| infer_provider_from_label(&reference.provider_label))
            .ok_or_else(|| {
                AppError::State(
                    "Connected shared storage profile is missing provider metadata and could not be inferred. Reconnect the provider."
                        .to_string(),
                )
            })?;

        let profile = SharedStorageProfile {
            id: reference.id.clone(),
            display_name: reference.display_name.clone(),
            provider: provider.clone(),
            provider_label: reference.provider_label.clone(),
            bucket: reference.bucket.clone(),
            prefix: reference.prefix.clone(),
            credential_vault_reference: format!(
                "state.json:sharedStorageCredentials.profiles.{}",
                reference.id
            ),
            repository_id: String::new(),
            status: SharedStorageStatus::Connected,
            last_verified_at: None,
            protected_bundles_count: 0,
            total_stored_bytes: 0,
        };
        drop(state);

        let manager = shared_profile_manager();
        let credentials = manager
            .retrieve_credentials(context, &profile)
            .await
            .map_err(|error| {
            AppError::State(format!(
                "Shared storage credentials for '{}' are unavailable. Reconnect this provider. {error}",
                profile.display_name
            ))
        })?;
        let provider_fields = manager.retrieve_provider_fields(context, &profile).await?;
        let remote_name = format!("noland_{}", profile.id.replace('-', ""));

        Ok(ActiveSharedStorageProfile {
            profile,
            credentials,
            provider_fields,
            remote_name,
        })
    }

    pub(crate) async fn configure_rclone_remote_for_profile(
        remote: &RemoteExec,
        target_user: &str,
        active_profile: &ActiveSharedStorageProfile,
    ) -> AppResult<()> {
        let rclone_conf_dir = format!("/home/{}/.config/rclone", target_user);
        let rclone_conf_path = format!("{}/rclone.conf", rclone_conf_dir);
        let mkdir_cmd = format!(
            "sudo -u {user} mkdir -p {dir}",
            user = target_user,
            dir = shell_escape(&rclone_conf_dir),
        );
        {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&mkdir_cmd, Duration::from_secs(15)))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??;
        }

        let config_content = Self::build_rclone_config_content(active_profile)?;
        let write_cmd = format!(
            "sudo -u {user} bash -lc 'cat > {path} <<\"RCLONE_EOF\"\n{content}\nRCLONE_EOF\nchmod 600 {path}'",
            user = target_user,
            path = shell_escape(&rclone_conf_path),
            content = config_content,
        );
        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&write_cmd, Duration::from_secs(30)))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };
        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed to write rclone config for {}: {} {}",
                active_profile.profile.provider.label(),
                output.stdout.trim(),
                output.stderr.trim()
            )));
        }
        Ok(())
    }

    fn build_rclone_config_content(
        active_profile: &ActiveSharedStorageProfile,
    ) -> AppResult<String> {
        let provider = &active_profile.profile.provider;
        let name = &active_profile.remote_name;
        let fields = &active_profile.provider_fields;
        let mut lines = vec![format!("[{}]", name)];

        match (provider, &active_profile.credentials) {
            (
                StorageProvider::BackblazeB2,
                StorageCredential::BackblazeB2 {
                    key_id,
                    application_key,
                },
            ) => {
                lines.push("type = b2".to_string());
                lines.push(format!("account = {}", key_id));
                lines.push(format!("key = {}", application_key));
            }
            (
                StorageProvider::AmazonS3,
                StorageCredential::S3 {
                    access_key_id,
                    secret_access_key,
                    session_token,
                },
            ) => {
                lines.push("type = s3".to_string());
                lines.push("provider = AWS".to_string());
                lines.push(format!("access_key_id = {}", access_key_id));
                lines.push(format!("secret_access_key = {}", secret_access_key));
                lines.push(format!(
                    "region = {}",
                    fields
                        .get("region")
                        .cloned()
                        .unwrap_or_else(|| "us-east-1".to_string())
                ));
                if let Some(token) = session_token.as_ref().filter(|v: &&String| !v.is_empty()) {
                    lines.push(format!("session_token = {}", token));
                }
            }
            (
                StorageProvider::CloudflareR2,
                StorageCredential::S3 {
                    access_key_id,
                    secret_access_key,
                    ..
                },
            ) => {
                let account_id = fields.get("account_id").cloned().ok_or_else(|| {
                    AppError::InvalidInput("Cloudflare R2 account_id is missing.".to_string())
                })?;
                lines.push("type = s3".to_string());
                lines.push("provider = Cloudflare".to_string());
                lines.push(format!("access_key_id = {}", access_key_id));
                lines.push(format!("secret_access_key = {}", secret_access_key));
                lines.push("region = auto".to_string());
                lines.push(format!(
                    "endpoint = https://{}.r2.cloudflarestorage.com",
                    account_id
                ));
            }
            (
                StorageProvider::Wasabi,
                StorageCredential::S3 {
                    access_key_id,
                    secret_access_key,
                    ..
                },
            ) => {
                lines.push("type = s3".to_string());
                lines.push("provider = Wasabi".to_string());
                lines.push(format!("access_key_id = {}", access_key_id));
                lines.push(format!("secret_access_key = {}", secret_access_key));
                lines.push(format!(
                    "region = {}",
                    fields
                        .get("region")
                        .cloned()
                        .unwrap_or_else(|| "us-east-1".to_string())
                ));
                if let Some(endpoint) = fields.get("endpoint").filter(|v| !v.trim().is_empty()) {
                    lines.push(format!("endpoint = {}", endpoint));
                }
            }
            (
                StorageProvider::DigitalOceanSpaces,
                StorageCredential::S3 {
                    access_key_id,
                    secret_access_key,
                    ..
                },
            ) => {
                let region = fields.get("region").cloned().ok_or_else(|| {
                    AppError::InvalidInput("DigitalOcean Spaces region is missing.".to_string())
                })?;
                lines.push("type = s3".to_string());
                lines.push("provider = DigitalOcean".to_string());
                lines.push(format!("access_key_id = {}", access_key_id));
                lines.push(format!("secret_access_key = {}", secret_access_key));
                lines.push(format!("region = {}", region));
                lines.push(format!("endpoint = {}.digitaloceanspaces.com", region));
            }
            (
                StorageProvider::GenericS3,
                StorageCredential::S3 {
                    access_key_id,
                    secret_access_key,
                    ..
                },
            ) => {
                lines.push("type = s3".to_string());
                lines.push("provider = Other".to_string());
                lines.push(format!("access_key_id = {}", access_key_id));
                lines.push(format!("secret_access_key = {}", secret_access_key));
                if let Some(endpoint) = fields.get("endpoint") {
                    lines.push(format!("endpoint = {}", endpoint));
                }
                lines.push(format!(
                    "region = {}",
                    fields
                        .get("region")
                        .cloned()
                        .unwrap_or_else(|| "auto".to_string())
                ));
                if let Some(v) = fields.get("force_path_style") {
                    lines.push(format!(
                        "force_path_style = {}",
                        if v == "true" || v == "1" {
                            "true"
                        } else {
                            "false"
                        }
                    ));
                }
            }
            (
                StorageProvider::GoogleDrive,
                StorageCredential::OAuth2 {
                    access_token,
                    refresh_token,
                    expires_at,
                },
            ) => {
                lines.push("type = drive".to_string());
                if let Some(client_id) = fields.get("client_id").filter(|v| !v.trim().is_empty()) {
                    lines.push(format!("client_id = {}", client_id));
                }
                if let Some(client_secret) =
                    fields.get("client_secret").filter(|v| !v.trim().is_empty())
                {
                    lines.push(format!("client_secret = {}", client_secret));
                }
                lines.push(format!(
                    "token = {}",
                    oauth_token_json(access_token, refresh_token.as_deref(), *expires_at)
                ));
            }
            (
                StorageProvider::MicrosoftOneDrive,
                StorageCredential::OAuth2 {
                    access_token,
                    refresh_token,
                    expires_at,
                },
            ) => {
                lines.push("type = onedrive".to_string());
                if let Some(client_id) = fields.get("client_id").filter(|v| !v.trim().is_empty()) {
                    lines.push(format!("client_id = {}", client_id));
                }
                if let Some(client_secret) =
                    fields.get("client_secret").filter(|v| !v.trim().is_empty())
                {
                    lines.push(format!("client_secret = {}", client_secret));
                }
                lines.push(format!(
                    "token = {}",
                    oauth_token_json(access_token, refresh_token.as_deref(), *expires_at)
                ));
            }
            (
                StorageProvider::Dropbox,
                StorageCredential::OAuth2 {
                    access_token,
                    refresh_token,
                    expires_at,
                },
            ) => {
                lines.push("type = dropbox".to_string());
                if let Some(client_id) = fields.get("client_id").filter(|v| !v.trim().is_empty()) {
                    lines.push(format!("client_id = {}", client_id));
                }
                if let Some(client_secret) =
                    fields.get("client_secret").filter(|v| !v.trim().is_empty())
                {
                    lines.push(format!("client_secret = {}", client_secret));
                }
                lines.push(format!(
                    "token = {}",
                    oauth_token_json(access_token, refresh_token.as_deref(), *expires_at)
                ));
            }
            (
                StorageProvider::Box,
                StorageCredential::OAuth2 {
                    access_token,
                    refresh_token,
                    expires_at,
                },
            ) => {
                lines.push("type = box".to_string());
                if let Some(client_id) = fields.get("client_id").filter(|v| !v.trim().is_empty()) {
                    lines.push(format!("client_id = {}", client_id));
                }
                if let Some(client_secret) =
                    fields.get("client_secret").filter(|v| !v.trim().is_empty())
                {
                    lines.push(format!("client_secret = {}", client_secret));
                }
                lines.push(format!(
                    "token = {}",
                    oauth_token_json(access_token, refresh_token.as_deref(), *expires_at)
                ));
            }
            (StorageProvider::GoogleCloudStorage, StorageCredential::ServiceAccount { json }) => {
                lines.push("type = google cloud storage".to_string());
                lines.push(format!("service_account_credentials = {}", json));
            }
            (
                StorageProvider::AzureBlob,
                StorageCredential::UsernamePassword { username, password },
            ) => {
                lines.push("type = azureblob".to_string());
                lines.push(format!("account = {}", username));
                lines.push(format!("key = {}", password));
            }
            (StorageProvider::Sftp, StorageCredential::UsernamePassword { username, password }) => {
                lines.push("type = sftp".to_string());
                lines.push(format!(
                    "host = {}",
                    fields
                        .get("host")
                        .cloned()
                        .ok_or_else(|| AppError::InvalidInput(
                            "SFTP host is missing.".to_string()
                        ))?
                ));
                lines.push(format!("user = {}", username));
                lines.push(format!("pass = {}", password));
                if let Some(port) = fields.get("port").filter(|v| !v.trim().is_empty()) {
                    lines.push(format!("port = {}", port));
                }
            }
            (
                StorageProvider::Webdav,
                StorageCredential::UsernamePassword { username, password },
            ) => {
                lines.push("type = webdav".to_string());
                lines.push(format!(
                    "url = {}",
                    fields
                        .get("url")
                        .cloned()
                        .ok_or_else(|| AppError::InvalidInput(
                            "WebDAV URL is missing.".to_string()
                        ))?
                ));
                lines.push(format!(
                    "vendor = {}",
                    fields
                        .get("vendor")
                        .cloned()
                        .unwrap_or_else(|| "other".to_string())
                ));
                lines.push(format!("user = {}", username));
                lines.push(format!("pass = {}", password));
            }
            _ => {
                return Err(AppError::InvalidInput(format!(
                    "Provider {} is not compatible with the stored credential format yet.",
                    provider.label()
                )));
            }
        }

        Ok(lines.join("\n"))
    }

    pub(crate) fn build_profile_storage_source(
        active_profile: &ActiveSharedStorageProfile,
    ) -> String {
        match &active_profile.profile.provider {
            StorageProvider::BackblazeB2
            | StorageProvider::AmazonS3
            | StorageProvider::CloudflareR2
            | StorageProvider::Wasabi
            | StorageProvider::DigitalOceanSpaces
            | StorageProvider::GenericS3
            | StorageProvider::GoogleCloudStorage
            | StorageProvider::AzureBlob => {
                let bucket = active_profile.profile.bucket.clone().unwrap_or_else(|| {
                    active_profile
                        .provider_fields
                        .get("bucket")
                        .cloned()
                        .or_else(|| active_profile.provider_fields.get("space_name").cloned())
                        .or_else(|| active_profile.provider_fields.get("container").cloned())
                        .unwrap_or_else(|| "noland".to_string())
                });
                let prefix = active_profile.profile.prefix.clone().unwrap_or_default();
                if prefix.trim().is_empty() {
                    format!("{}:{}", active_profile.remote_name, bucket)
                } else {
                    format!(
                        "{}:{}/{}",
                        active_profile.remote_name,
                        bucket,
                        prefix.trim_matches('/')
                    )
                }
            }
            _ => {
                let prefix = active_profile
                    .profile
                    .prefix
                    .clone()
                    .or_else(|| active_profile.provider_fields.get("folder").cloned())
                    .or_else(|| active_profile.provider_fields.get("remote_path").cloned())
                    .unwrap_or_else(|| "noland".to_string());
                if prefix.trim().is_empty() {
                    format!("{}:", active_profile.remote_name)
                } else {
                    format!(
                        "{}:{}",
                        active_profile.remote_name,
                        prefix.trim_matches('/')
                    )
                }
            }
        }
    }

    /// Ensure rclone is installed on the VM.
    async fn ensure_rclone_installed(remote: &RemoteExec) -> AppResult<()> {
        let check = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh("command -v rclone >/dev/null 2>&1 && echo 'RCLONE_OK' || echo 'RCLONE_MISSING'", Duration::from_secs(15))
            })
            .await
            .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };

        if check.stdout.contains("RCLONE_OK") {
            return Ok(());
        }

        info!("rclone not found, installing...");

        let install = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || {
                remote.ssh(
                    "curl -fsSL https://rclone.org/install.sh | sudo bash && echo 'RCLONE_INSTALLED'",
                    Duration::from_secs(120),
                )
            })
            .await
            .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };

        if install.status_code != 0 || !install.stdout.contains("RCLONE_INSTALLED") {
            return Err(AppError::Provisioning(format!(
                "Failed to install rclone: stdout: {} | stderr: {}",
                install.stdout.trim(),
                install.stderr.trim()
            )));
        }

        info!("rclone installed successfully");
        Ok(())
    }

    /// Write the rclone filter rules file to the VM.
    async fn write_filter_rules(remote: &RemoteExec, target_user: &str) -> AppResult<()> {
        let rules_content = include_str!("../../../scripts/shared_storage_rules.txt");
        let rules_path = format!("/home/{}/rules.txt", target_user);

        let cmd = format!(
            "sudo -u {user} bash -lc 'cat > {path} <<\"RULES_EOF\"\n{content}\nRULES_EOF\nchmod 644 {path}'",
            user = target_user,
            path = shell_escape(&rules_path),
            content = rules_content,
        );

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&cmd, Duration::from_secs(30)))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };

        info!(
            event = "shared_storage_write_filter_rules_output",
            target_user = target_user,
            status_code = output.status_code,
            stdout = %output.stdout.trim(),
            stderr = %output.stderr.trim(),
            "Shared storage filter-rules write command output"
        );

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed to write filter rules: stdout: {} | stderr: {}",
                output.stdout.trim(),
                output.stderr.trim()
            )));
        }

        info!("Filter rules written to {}", rules_path);
        Ok(())
    }

    /// Configure the rclone remote for Backblaze B2.
    /// Writes config directly to file to avoid exposing secrets in CLI args / process lists.
    async fn configure_rclone_remote(
        remote: &RemoteExec,
        target_user: &str,
        settings: &crate::models::app_state::SharedStorageSettings,
    ) -> AppResult<()> {
        let rclone_conf_dir = format!("/home/{}/.config/rclone", target_user);
        let rclone_conf_path = format!("{}/rclone.conf", rclone_conf_dir);

        // Ensure config directory exists
        let mkdir_cmd = format!(
            "sudo -u {user} mkdir -p {dir}",
            user = target_user,
            dir = shell_escape(&rclone_conf_dir),
        );

        let _ = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&mkdir_cmd, Duration::from_secs(15)))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };

        // Build rclone config file content directly (no secrets in process list)
        let config_content = format!(
            "[{name}]\ntype = b2\naccount = {account}\nkey = {key}\n\n[{crypt_name}]\ntype = crypt\nremote = {name}:{bucket}\nfilename_encryption = off\ndirectory_name_encryption = false\npassword = {crypt_pass}\n",
            name = settings.remote_name,
            account = settings.backblaze_key_id,
            key = settings.backblaze_application_key,
            crypt_name = crypt_remote_name(&settings.remote_name),
            bucket = settings.bucket_name,
            crypt_pass = settings.crypt_password.as_deref().unwrap_or(""),
        );

        let write_cmd = format!(
            "sudo -u {user} bash -lc 'cat > {path} <<\"RCLONE_EOF\"\n{content}\nRCLONE_EOF\nchmod 600 {path}'",
            user = target_user,
            path = shell_escape(&rclone_conf_path),
            content = config_content,
        );

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&write_cmd, Duration::from_secs(30)))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };

        if output.status_code != 0 {
            let redacted = redact_secrets(&output.stdout, settings);
            return Err(AppError::Provisioning(format!(
                "Failed to write rclone config: {}",
                redacted.trim()
            )));
        }

        info!(
            "rclone remote '{}' configured (secrets written directly to config file)",
            settings.remote_name
        );
        Ok(())
    }

    /// Determine the effective remote name for backups.
    /// If crypt password is set, uses the crypt overlay; otherwise uses plain B2.
    fn effective_remote_name(settings: &crate::models::app_state::SharedStorageSettings) -> String {
        if settings
            .crypt_password
            .as_deref()
            .map(|s| !s.is_empty())
            .unwrap_or(false)
        {
            crypt_remote_name(&settings.remote_name)
        } else {
            settings.remote_name.clone()
        }
    }

    fn build_storage_source(
        settings: &crate::models::app_state::SharedStorageSettings,
        force_plain: bool,
    ) -> String {
        let effective_remote = if force_plain {
            settings.remote_name.clone()
        } else {
            Self::effective_remote_name(settings)
        };

        if !force_plain
            && settings
                .crypt_password
                .as_deref()
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false)
        {
            format!("{}:{}", effective_remote, settings.destination_prefix)
        } else {
            format!(
                "{}:{}/{}",
                effective_remote, settings.bucket_name, settings.destination_prefix
            )
        }
    }

    /// Set up the hourly backup schedule via cron.
    pub async fn setup_scheduled_backup(
        context: &AppContext,
        remote: &RemoteExec,
        instance_id: u64,
        target_user: &str,
    ) -> AppResult<()> {
        let _ = (context, remote, instance_id, target_user);
        Err(AppError::InvalidInput(
            Self::SCHEDULED_BACKUPS_DISABLED_MESSAGE.to_string(),
        ))
    }

    /// Remove the scheduled backup cron entry.
    pub async fn remove_scheduled_backup(remote: &RemoteExec, target_user: &str) -> AppResult<()> {
        let _ = (remote, target_user);
        Err(AppError::InvalidInput(
            Self::SCHEDULED_BACKUPS_DISABLED_MESSAGE.to_string(),
        ))
    }

    /// Auto-restore backup contents from B2 into the VM after post-provision.
    ///
    /// Non-blocking by design from orchestration's perspective: callers can log
    /// and continue if this returns an error. This method itself is best-effort
    /// and skips silently when storage is not configured or no remote data exists.
    pub async fn auto_restore_instance(
        context: &AppContext,
        remote: &RemoteExec,
        instance_id: u64,
        target_user: &str,
    ) -> AppResult<()> {
        let _ = (context, remote, target_user);
        info!(
            instance_id = instance_id,
            "Skipping shared-storage auto-restore because only explicit user-selected transfers are allowed"
        );
        Ok(())
    }

    /// Sync current VM state to B2 in destructive mode for teardown.
    ///
    /// This is used before destroy so files deleted on VM are deleted in B2.
    /// Get the backup status for the current app state.
    pub async fn get_backup_status(
        context: &AppContext,
    ) -> AppResult<crate::models::app_state::BackupStatusResponse> {
        let state = context.load_state().await;
        let ss = &state.shared_storage;
        Ok(crate::models::app_state::BackupStatusResponse {
            last_backup_started_at: ss.last_backup_started_at.clone(),
            last_backup_finished_at: ss.last_backup_finished_at.clone(),
            last_backup_status: ss.last_backup_status.clone(),
            last_backup_error: ss.last_backup_error.clone(),
            last_backup_trigger: ss.last_backup_trigger.clone(),
        })
    }

    /// Get per-instance backup status (including whether a backup is currently running).
    pub async fn get_instance_backup_status(
        context: &AppContext,
        instance_id: u64,
    ) -> AppResult<crate::models::app_state::SharedStorageInstanceStatus> {
        let state = context.load_state().await;
        let ss = &state.shared_storage;

        let running = {
            let map = get_running_backups().read().await;
            map.contains_key(&instance_id)
        };

        Ok(crate::models::app_state::SharedStorageInstanceStatus {
            instance_id,
            backup_running: running,
            last_backup_started_at: ss.last_backup_started_at.clone(),
            last_backup_finished_at: ss.last_backup_finished_at.clone(),
            last_backup_status: ss.last_backup_status.clone(),
            last_backup_error: ss.last_backup_error.clone(),
        })
    }
}

/// Simple shell escaping for single-quoted strings.
fn shell_escape(input: &str) -> String {
    input.replace('\'', "'\"'\"'")
}

fn split_parent_and_name(path: &str) -> (String, String) {
    let trimmed = path.trim_matches('/');
    if trimmed.is_empty() {
        return ("/".to_string(), "/".to_string());
    }

    if let Some((parent, name)) = trimmed.rsplit_once('/') {
        (format!("/{parent}"), name.to_string())
    } else {
        ("/".to_string(), trimmed.to_string())
    }
}

fn parent_dir(path: &str) -> String {
    let trimmed = path.trim_matches('/');
    if let Some((parent, _)) = trimmed.rsplit_once('/') {
        if parent.is_empty() {
            "/".to_string()
        } else {
            format!("/{parent}")
        }
    } else {
        "/".to_string()
    }
}

fn normalize_selection_path(path: &str) -> AppResult<String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(AppError::InvalidInput(
            "Selected sync path cannot be empty.".to_string(),
        ));
    }

    if trimmed.contains("..") {
        return Err(AppError::InvalidInput(
            "Selected sync path cannot contain '..'.".to_string(),
        ));
    }

    let normalized = format!("/{}", trimmed.trim_start_matches('/').trim_end_matches('/'));
    Ok(normalized)
}

fn listing_cache_key(
    remote: &RemoteExec,
    target_user: &str,
    active_profile: &ActiveSharedStorageProfile,
) -> String {
    format!(
        "{}:{}:{}:{}:{}:{}",
        remote.ssh_host,
        remote.ssh_port,
        target_user,
        active_profile.remote_name,
        active_profile.profile.id,
        active_profile.profile.provider.label(),
    )
}

fn infer_provider_from_label(label: &str) -> Option<StorageProvider> {
    let normalized = label.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "amazon s3" => Some(StorageProvider::AmazonS3),
        "backblaze b2" => Some(StorageProvider::BackblazeB2),
        "cloudflare r2" => Some(StorageProvider::CloudflareR2),
        "wasabi" => Some(StorageProvider::Wasabi),
        "digitalocean spaces" => Some(StorageProvider::DigitalOceanSpaces),
        "generic s3" => Some(StorageProvider::GenericS3),
        "google drive" => Some(StorageProvider::GoogleDrive),
        "google cloud storage" => Some(StorageProvider::GoogleCloudStorage),
        "microsoft onedrive" => Some(StorageProvider::MicrosoftOneDrive),
        "dropbox" => Some(StorageProvider::Dropbox),
        "box" => Some(StorageProvider::Box),
        "azure blob storage" => Some(StorageProvider::AzureBlob),
        "sftp" => Some(StorageProvider::Sftp),
        "webdav" => Some(StorageProvider::Webdav),
        _ => None,
    }
}

fn extract_actionable_rclone_error(output: &str) -> Option<String> {
    let known_host_noise = "warning: permanently added";
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower.contains(known_host_noise) {
            continue;
        }
        if lower.contains("failed to")
            || lower.contains("permission denied")
            || lower.contains("critical:")
            || lower.contains("error")
        {
            return Some(trimmed.to_string());
        }
    }
    None
}

/// Generate the crypt remote overlay name from the base remote name.
fn crypt_remote_name(base: &str) -> String {
    format!("{}-crypt", base)
}

fn redact_secrets(
    input: &str,
    settings: &crate::models::app_state::SharedStorageSettings,
) -> String {
    let mut result = input.to_string();
    if !settings.backblaze_application_key.is_empty() {
        result = result.replace(&settings.backblaze_application_key, "***REDACTED***");
    }
    if !settings.backblaze_key_id.is_empty() {
        result = result.replace(&settings.backblaze_key_id, "***REDACTED***");
    }
    if let Some(ref pwd) = settings.crypt_password {
        if !pwd.is_empty() {
            result = result.replace(pwd, "***REDACTED***");
        }
    }
    result
}

fn oauth_token_json(access_token: &str, refresh_token: Option<&str>, expires_at: i64) -> String {
    let expiry = chrono::DateTime::<chrono::Utc>::from_timestamp(expires_at, 0)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339();
    serde_json::json!({
        "access_token": access_token,
        "refresh_token": refresh_token,
        "expiry": expiry,
    })
    .to_string()
}

fn redact_profile_secrets(input: &str, active_profile: &ActiveSharedStorageProfile) -> String {
    let mut result = input.to_string();

    match &active_profile.credentials {
        StorageCredential::BackblazeB2 {
            key_id,
            application_key,
        } => {
            if !key_id.is_empty() {
                result = result.replace(key_id, "***REDACTED***");
            }
            if !application_key.is_empty() {
                result = result.replace(application_key, "***REDACTED***");
            }
        }
        StorageCredential::S3 {
            access_key_id,
            secret_access_key,
            session_token,
        } => {
            if !access_key_id.is_empty() {
                result = result.replace(access_key_id, "***REDACTED***");
            }
            if !secret_access_key.is_empty() {
                result = result.replace(secret_access_key, "***REDACTED***");
            }
            if let Some(session_token) = session_token.as_ref().filter(|v: &&String| !v.is_empty())
            {
                result = result.replace(session_token, "***REDACTED***");
            }
        }
        StorageCredential::OAuth2 {
            access_token,
            refresh_token,
            ..
        } => {
            if !access_token.is_empty() {
                result = result.replace(access_token, "***REDACTED***");
            }
            if let Some(refresh_token) = refresh_token.as_ref().filter(|v: &&String| !v.is_empty())
            {
                result = result.replace(refresh_token, "***REDACTED***");
            }
        }
        StorageCredential::UsernamePassword { username, password } => {
            if !username.is_empty() {
                result = result.replace(username, "***REDACTED***");
            }
            if !password.is_empty() {
                result = result.replace(password, "***REDACTED***");
            }
        }
        StorageCredential::SshKey {
            username,
            private_key,
            passphrase,
        } => {
            if !username.is_empty() {
                result = result.replace(username, "***REDACTED***");
            }
            if !private_key.is_empty() {
                result = result.replace(private_key, "***REDACTED***");
            }
            if let Some(passphrase) = passphrase.as_ref().filter(|v: &&String| !v.is_empty()) {
                result = result.replace(passphrase, "***REDACTED***");
            }
        }
        StorageCredential::ServiceAccount { json } => {
            if !json.is_empty() {
                result = result.replace(json, "***REDACTED***");
            }
        }
    }

    for value in active_profile.provider_fields.values() {
        if !value.is_empty()
            && (value.contains("secret") || value.contains('{') || value.len() > 24)
        {
            result = result.replace(value, "***REDACTED***");
        }
    }

    result
}
