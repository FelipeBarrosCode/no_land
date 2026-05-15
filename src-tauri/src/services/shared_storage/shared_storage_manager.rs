use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use tokio::sync::RwLock;
use tracing::{error, info, warn};

use crate::errors::{AppError, AppResult};

use crate::services::{app_context::AppContext, remote_exec::RemoteExec};

use super::bundle_indexer::BundleIndexer;

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

/// Shared Storage Manager service.
///
/// Handles Backblaze B2 backup configuration, manual/scheduled backup
/// triggering, and status tracking for provisioned VM instances.
pub struct SharedStorageManager;

impl SharedStorageManager {
    pub async fn list_local_objects(
        context: &AppContext,
        remote: &RemoteExec,
        target_user: &str,
    ) -> AppResult<Vec<crate::models::app_state::SharedStorageObjectEntry>> {
        let state = context.load_state().await;
        let settings = &state.shared_storage.settings;

        if !settings.enabled {
            return Err(AppError::InvalidInput(
                "Shared storage is disabled. Enable it in settings first.".to_string(),
            ));
        }

        if settings.backblaze_key_id.trim().is_empty()
            || settings.backblaze_application_key.trim().is_empty()
        {
            return Err(AppError::InvalidInput(
                "Backblaze credentials are missing. Save shared storage settings first."
                    .to_string(),
            ));
        }

        Self::ensure_rclone_installed(remote).await?;
        Self::write_filter_rules(remote, target_user).await?;
        Self::configure_rclone_remote(remote, target_user, settings).await?;

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

        let state = context.load_state().await;
        let settings = &state.shared_storage.settings;

        Self::ensure_rclone_installed(remote).await?;
        Self::configure_rclone_remote(remote, target_user, settings).await?;

        let dest = Self::build_storage_source(settings, false);
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
            let combined = format!("{}\n{}", output.stdout, output.stderr);
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
        let state = context.load_state().await;
        let settings = &state.shared_storage.settings;

        if !settings.enabled {
            return Err(AppError::InvalidInput(
                "Shared storage is disabled. Enable it in settings first.".to_string(),
            ));
        }

        if settings.backblaze_key_id.trim().is_empty()
            || settings.backblaze_application_key.trim().is_empty()
        {
            return Err(AppError::InvalidInput(
                "Backblaze credentials are missing. Save shared storage settings first."
                    .to_string(),
            ));
        }

        let cache_key = listing_cache_key(remote, target_user, settings);
        let cached_ready = {
            let cache = get_listing_ready_cache().read().await;
            cache.contains(&cache_key)
        };

        if !cached_ready {
            let setup_started = Instant::now();
            Self::ensure_rclone_installed(remote).await?;
            Self::configure_rclone_remote(remote, target_user, settings).await?;
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

        let source = Self::build_storage_source(settings, false);
        info!(source = source, "shared-storage list source resolved");

        let list_cmd = format!(
            "sudo -u {user} rclone lsf {src} --recursive --files-only --fast-list",
            user = target_user,
            src = shell_escape(&source),
        );

        let listing_started = Instant::now();
        let first_attempt = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh_until_complete(&list_cmd))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };

        info!(
            status_code = first_attempt.status_code,
            stdout_len = first_attempt.stdout.len(),
            stderr_len = first_attempt.stderr.len(),
            elapsed_ms = listing_started.elapsed().as_millis() as u64,
            "shared-storage list_remote_objects command finished"
        );

        let lower_error_blob = format!(
            "{}\n{}",
            first_attempt.stdout.to_ascii_lowercase(),
            first_attempt.stderr.to_ascii_lowercase()
        );

        let needs_plain_fallback = first_attempt.status_code != 0
            && (lower_error_blob.contains("password not set in config file")
                || lower_error_blob.contains("failed to create file system for")
                || lower_error_blob.contains("crypt"));

        let output = if needs_plain_fallback {
            warn!("shared-storage list failed on crypt remote, retrying with plain B2 remote");
            {
                let mut cache = get_listing_ready_cache().write().await;
                cache.remove(&cache_key);
            }

            let reconfigure_started = Instant::now();
            Self::configure_rclone_remote(remote, target_user, settings).await?;
            info!(
                elapsed_ms = reconfigure_started.elapsed().as_millis() as u64,
                "shared-storage list reconfigured rclone after failure"
            );

            let fallback_source = Self::build_storage_source(settings, true);
            info!(
                source = fallback_source,
                "shared-storage list fallback source resolved"
            );
            let fallback_cmd = format!(
                "sudo -u {user} rclone lsf {src} --recursive --files-only --fast-list",
                user = target_user,
                src = shell_escape(&fallback_source),
            );
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh_until_complete(&fallback_cmd))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        } else {
            first_attempt
        };

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed listing Backblaze objects: {}",
                redact_secrets(&format!("{}\n{}", output.stdout, output.stderr), settings).trim()
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

        let state = context.load_state().await;
        let settings = &state.shared_storage.settings;

        if !settings.enabled {
            return Err(AppError::InvalidInput(
                "Shared storage is disabled. Enable it in settings first.".to_string(),
            ));
        }

        if settings.backblaze_key_id.trim().is_empty()
            || settings.backblaze_application_key.trim().is_empty()
        {
            return Err(AppError::InvalidInput(
                "Backblaze credentials are missing. Save shared storage settings first."
                    .to_string(),
            ));
        }

        Self::ensure_rclone_installed(remote).await?;
        Self::write_filter_rules(remote, target_user).await?;
        Self::configure_rclone_remote(remote, target_user, settings).await?;

        let source = Self::build_storage_source(settings, false);

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
                redact_secrets(&restore.stderr, settings).trim()
            )));
        }

        info!(
            instance_id = instance_id,
            count = selected_paths.len(),
            "Selective shared storage sync completed"
        );
        Ok(format!(
            "Synced {} selected items from Backblaze",
            selected_paths.len()
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
        info!(
            event = "shared_storage_test_start",
            target_user = target_user,
            "Shared storage configuration test started"
        );
        let state = context.load_state().await;
        let settings = &state.shared_storage.settings;

        if settings.backblaze_key_id.trim().is_empty()
            || settings.backblaze_application_key.trim().is_empty()
        {
            return Err(AppError::Provisioning(
                "Backblaze credentials are missing. Please configure Key ID and Application Key."
                    .to_string(),
            ));
        }

        // Ensure rclone is installed
        Self::ensure_rclone_installed(remote).await?;

        // Configure the remote
        Self::configure_rclone_remote(remote, target_user, settings).await?;

        // Test with a lightweight list
        let effective_remote = Self::effective_remote_name(settings);
        let list_cmd = format!(
            "sudo -u {user} rclone ls {remote}:{bucket} --max-depth 1 2>&1 | head -5",
            user = target_user,
            remote = shell_escape(&effective_remote),
            bucket = shell_escape(&settings.bucket_name),
        );

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&list_cmd, Duration::from_secs(60)))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };

        let test_stdout = redact_secrets(&output.stdout, settings);
        let test_stderr = redact_secrets(&output.stderr, settings);
        info!(
            event = "shared_storage_test_output",
            target_user = target_user,
            status_code = output.status_code,
            stdout = %test_stdout.trim(),
            stderr = %test_stderr.trim(),
            "Shared storage test command output"
        );

        if output.status_code != 0 {
            let stderr = redact_secrets(&output.stdout, settings);
            warn!(
                event = "shared_storage_test_failure",
                target_user = target_user,
                status_code = output.status_code,
                "Shared storage configuration test failed"
            );
            return Err(AppError::Provisioning(format!(
                "Backblaze B2 connection test failed: {}",
                stderr.trim()
            )));
        }

        info!(
            event = "shared_storage_test_success",
            target_user = target_user,
            "Shared storage configuration test succeeded"
        );
        info!("Backblaze B2 configuration test succeeded");
        Ok(())
    }

    /// Trigger a manual backup for the given instance.
    pub async fn trigger_manual_backup(
        context: &AppContext,
        remote: &RemoteExec,
        instance_id: u64,
        target_user: &str,
    ) -> AppResult<()> {
        Self::trigger_backup(context, remote, instance_id, target_user, "manual").await
    }

    /// Internal backup trigger with concurrency guard.
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

        let state = context.load_state().await;
        let settings = &state.shared_storage.settings;

        if !settings.enabled {
            return Err(AppError::Provisioning(
                "Shared storage backup is not enabled. Please configure settings first."
                    .to_string(),
            ));
        }

        if settings.backblaze_key_id.trim().is_empty()
            || settings.backblaze_application_key.trim().is_empty()
        {
            return Err(AppError::Provisioning(
                "Backblaze credentials are missing.".to_string(),
            ));
        }

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

        let result = Self::run_backup(remote, target_user, settings, trigger).await;

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
    async fn run_backup(
        remote: &RemoteExec,
        target_user: &str,
        settings: &crate::models::app_state::SharedStorageSettings,
        trigger: &str,
    ) -> AppResult<()> {
        // Ensure rclone is installed
        Self::ensure_rclone_installed(remote).await?;

        // Write filter rules
        Self::write_filter_rules(remote, target_user).await?;

        // Configure rclone remote
        Self::configure_rclone_remote(remote, target_user, settings).await?;

        let filter_path = format!("/home/{}/rules.txt", target_user);
        let rclone_config_path = format!("/home/{}/.config/rclone/rclone.conf", target_user);
        let dest = Self::build_storage_source(settings, false);

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

        let stdout = redact_secrets(&output.stdout, settings);
        let stderr = redact_secrets(&output.stderr, settings);

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
                    "Backblaze storage cap exceeded. Increase your B2 cap/quota in Caps & Alerts, then retry Save."
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
        info!(
            event = "shared_storage_schedule_setup_start",
            instance_id = instance_id,
            target_user = target_user,
            "Shared storage schedule setup started"
        );
        let state = context.load_state().await;
        let settings = &state.shared_storage.settings;

        if !settings.enabled {
            return Err(AppError::Provisioning(
                "Shared storage backup is not enabled.".to_string(),
            ));
        }

        // Ensure rclone is installed and remote is configured
        Self::ensure_rclone_installed(remote).await?;
        Self::write_filter_rules(remote, target_user).await?;
        Self::configure_rclone_remote(remote, target_user, settings).await?;

        let filter_path = format!("/home/{}/rules.txt", target_user);
        let dest = Self::build_storage_source(settings, false);

        // Create cron entry for the user
        let cron_cmd = format!(
            "0 * * * * rclone copy / {dest} --filter-from {filter} --checksum",
            dest = shell_escape(&dest),
            filter = shell_escape(&filter_path),
        );

        let cmd = format!(
            "(crontab -u {user} -l 2>/dev/null | grep -v 'noland-backup' || true; echo '{cron}') | crontab -u {user} -",
            user = target_user,
            cron = cron_cmd,
        );

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&cmd, Duration::from_secs(30)))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };

        info!(
            event = "shared_storage_schedule_setup_output",
            target_user = target_user,
            status_code = output.status_code,
            stdout = %output.stdout.trim(),
            stderr = %output.stderr.trim(),
            "Shared storage schedule setup command output"
        );

        if output.status_code != 0 {
            warn!(
                event = "shared_storage_schedule_setup_failure",
                instance_id = instance_id,
                target_user = target_user,
                status_code = output.status_code,
                "Shared storage schedule setup failed"
            );
            return Err(AppError::Provisioning(format!(
                "Failed to configure cron schedule: stdout: {} | stderr: {}",
                output.stdout.trim(),
                output.stderr.trim()
            )));
        }

        info!(
            event = "shared_storage_schedule_setup_success",
            instance_id = instance_id,
            target_user = target_user,
            "Shared storage schedule setup succeeded"
        );
        info!(
            instance_id = instance_id,
            "Hourly backup schedule configured; output now goes to system cron handling"
        );
        Ok(())
    }

    /// Remove the scheduled backup cron entry.
    pub async fn remove_scheduled_backup(remote: &RemoteExec, target_user: &str) -> AppResult<()> {
        info!(
            event = "shared_storage_schedule_remove_start",
            target_user = target_user,
            "Shared storage schedule removal started"
        );
        let cmd = format!(
            "crontab -u {user} -l 2>/dev/null | grep -v 'noland-backup' | crontab -u {user} -",
            user = target_user,
        );

        let output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&cmd, Duration::from_secs(30)))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };

        info!(
            event = "shared_storage_schedule_remove_output",
            target_user = target_user,
            status_code = output.status_code,
            stdout = %output.stdout.trim(),
            stderr = %output.stderr.trim(),
            "Shared storage schedule removal command output"
        );

        if output.status_code != 0 {
            warn!(
                event = "shared_storage_schedule_remove_failure",
                target_user = target_user,
                status_code = output.status_code,
                "Failed to remove cron schedule (may not exist): stdout: {} | stderr: {}",
                output.stdout.trim(),
                output.stderr.trim()
            );
        } else {
            info!(
                event = "shared_storage_schedule_remove_success",
                target_user = target_user,
                "Shared storage schedule removed"
            );
            info!("Hourly backup schedule removed");
        }

        Ok(())
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
        let state = context.load_state().await;
        let settings = &state.shared_storage.settings;

        if !settings.enabled {
            info!(
                instance_id = instance_id,
                "Shared storage disabled; skipping auto-restore"
            );
            return Ok(());
        }

        if settings.backblaze_key_id.trim().is_empty()
            || settings.backblaze_application_key.trim().is_empty()
        {
            info!(
                instance_id = instance_id,
                "Backblaze credentials missing; skipping auto-restore"
            );
            return Ok(());
        }

        Self::ensure_rclone_installed(remote).await?;
        Self::write_filter_rules(remote, target_user).await?;
        Self::configure_rclone_remote(remote, target_user, settings).await?;

        let source = Self::build_storage_source(settings, false);
        let filter_path = format!("/home/{}/rules.txt", target_user);

        // Check if backup content exists; skip silently when empty.
        let check_cmd = format!(
            "sudo -u {user} rclone lsf {src} --max-depth 1 2>&1",
            user = target_user,
            src = shell_escape(&source),
        );

        let check = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&check_cmd, Duration::from_secs(60)))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };

        if check.status_code != 0 {
            warn!(
                instance_id = instance_id,
                "Auto-restore source check failed; skipping. stdout={} stderr={}",
                check.stdout.trim(),
                check.stderr.trim()
            );
            return Ok(());
        }

        if check.stdout.trim().is_empty() {
            info!(
                instance_id = instance_id,
                "No prior backup contents found; skipping auto-restore"
            );
            return Ok(());
        }

        let restore_cmd = format!(
            "sudo -u {user} rclone copy {src} / --filter-from {filter} --checksum --update 2>&1",
            user = target_user,
            src = shell_escape(&source),
            filter = shell_escape(&filter_path),
        );

        let restore = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&restore_cmd, Duration::from_secs(3600)))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };

        if restore.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Auto-restore failed: {}",
                redact_secrets(&restore.stdout, settings).trim()
            )));
        }

        info!(
            instance_id = instance_id,
            "Auto-restore completed successfully"
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
    settings: &crate::models::app_state::SharedStorageSettings,
) -> String {
    format!(
        "{}:{}:{}:{}:{}:{}:{}",
        remote.ssh_host,
        remote.ssh_port,
        target_user,
        settings.remote_name,
        settings.bucket_name,
        settings.destination_prefix,
        settings.backblaze_key_id
    )
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

/// Redact secrets from a string before logging.
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
