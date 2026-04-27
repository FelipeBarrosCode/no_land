use std::{collections::HashMap, time::Duration};

use tokio::sync::RwLock;
use tracing::{error, info, warn};

use crate::errors::{AppError, AppResult};

use crate::services::{
    app_context::AppContext,
    remote_exec::RemoteExec,
};

use super::bundle_indexer::BundleIndexer;

/// In-memory tracking of running backups per instance to prevent overlap.
static RUNNING_BACKUPS: std::sync::OnceLock<RwLock<HashMap<u64, BackupJobInfo>>> =
    std::sync::OnceLock::new();

fn get_running_backups() -> &'static RwLock<HashMap<u64, BackupJobInfo>> {
    RUNNING_BACKUPS.get_or_init(|| RwLock::new(HashMap::new()))
}

#[derive(Debug, Clone)]
struct BackupJobInfo {
    started_at: String,
    trigger: String,
}

/// Shared Storage Manager service.
///
/// Handles Backblaze B2 backup configuration, manual/scheduled backup
/// triggering, and status tracking for provisioned VM instances.
pub struct SharedStorageManager;

impl SharedStorageManager {
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
    pub async fn get_settings(context: &AppContext) -> AppResult<crate::models::app_state::SharedStorageSettingsResponse> {
        let state = context.load_state().await;
        let s = &state.shared_storage.settings;
        Ok(crate::models::app_state::SharedStorageSettingsResponse {
            enabled: s.enabled,
            backblaze_key_id: s.backblaze_key_id.clone(),
            bucket_name: s.bucket_name.clone(),
            remote_name: s.remote_name.clone(),
            destination_prefix: s.destination_prefix.clone(),
            crypt_password_set: s.crypt_password.as_deref().map(|p| !p.is_empty()).unwrap_or(false),
        })
    }

    /// Test the Backblaze B2 configuration by creating the rclone remote
    /// and running a lightweight `rclone ls`.
    pub async fn test_configuration(
        context: &AppContext,
        remote: &RemoteExec,
        target_user: &str,
    ) -> AppResult<()> {
        let state = context.load_state().await;
        let settings = &state.shared_storage.settings;

        if settings.backblaze_key_id.trim().is_empty() || settings.backblaze_application_key.trim().is_empty() {
            return Err(AppError::Provisioning(
                "Backblaze credentials are missing. Please configure Key ID and Application Key.".to_string(),
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

        if output.status_code != 0 {
            let stderr = redact_secrets(&output.stdout, settings);
            return Err(AppError::Provisioning(format!(
                "Backblaze B2 connection test failed: {}",
                stderr.trim()
            )));
        }

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
                "Shared storage backup is not enabled. Please configure settings first.".to_string(),
            ));
        }

        if settings.backblaze_key_id.trim().is_empty() || settings.backblaze_application_key.trim().is_empty() {
            return Err(AppError::Provisioning(
                "Backblaze credentials are missing.".to_string(),
            ));
        }

        // Mark backup as running
        let started_at = chrono::Local::now().to_rfc3339();
        {
            let mut running = get_running_backups().write().await;
            running.insert(
                instance_id,
                BackupJobInfo {
                    started_at: started_at.clone(),
                    trigger: trigger.to_string(),
                },
            );
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
                if let Err(error) = BundleIndexer::generate_and_upload(
                    context,
                    remote,
                    instance_id,
                    target_user,
                )
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
                    instance_id = instance_id,
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
                    instance_id = instance_id,
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
        let effective_remote = Self::effective_remote_name(settings);
        let dest = format!(
            "{}:{}/{}",
            effective_remote, settings.bucket_name, settings.destination_prefix
        );

        let progress_flag = if trigger == "manual" { " --progress" } else { "" };
        let cmd = format!(
            "sudo -u {user} rclone copy / {dest} --filter-from {filter} --checksum{progress} 2>&1",
            user = target_user,
            dest = shell_escape(&dest),
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
            tokio::task::spawn_blocking(move || remote.ssh(&cmd, Duration::from_secs(3600)))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };

        let stdout = redact_secrets(&output.stdout, settings);

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "rclone sync failed (exit {}): {}",
                output.status_code,
                stdout.trim()
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

        info!("rclone remote '{}' configured (secrets written directly to config file)", settings.remote_name);
        Ok(())
    }

    /// Determine the effective remote name for backups.
    /// If crypt password is set, uses the crypt overlay; otherwise uses plain B2.
    fn effective_remote_name(settings: &crate::models::app_state::SharedStorageSettings) -> String {
        if settings.crypt_password.as_deref().map(|s| !s.is_empty()).unwrap_or(false) {
            crypt_remote_name(&settings.remote_name)
        } else {
            settings.remote_name.clone()
        }
    }

    /// Set up the hourly backup schedule via cron.
    pub async fn setup_scheduled_backup(
        context: &AppContext,
        remote: &RemoteExec,
        instance_id: u64,
        target_user: &str,
    ) -> AppResult<()> {
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
        let effective_remote = Self::effective_remote_name(settings);
        let dest = format!(
            "{}:{}/{}",
            effective_remote, settings.bucket_name, settings.destination_prefix
        );

        // Create cron entry for the user
        let cron_cmd = format!(
            "0 * * * * rclone copy / {dest} --filter-from {filter} --checksum >> /tmp/noland-backup.log 2>&1",
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

        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Failed to configure cron schedule: stdout: {} | stderr: {}",
                output.stdout.trim(),
                output.stderr.trim()
            )));
        }

        info!(instance_id = instance_id, "Hourly backup schedule configured");
        Ok(())
    }

    /// Remove the scheduled backup cron entry.
    pub async fn remove_scheduled_backup(
        remote: &RemoteExec,
        target_user: &str,
    ) -> AppResult<()> {
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

        if output.status_code != 0 {
            warn!(
                "Failed to remove cron schedule (may not exist): stdout: {} | stderr: {}",
                output.stdout.trim(),
                output.stderr.trim()
            );
        } else {
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
            info!(instance_id = instance_id, "Shared storage disabled; skipping auto-restore");
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

        let effective_remote = Self::effective_remote_name(settings);
        let source = format!(
            "{}:{}/{}",
            effective_remote, settings.bucket_name, settings.destination_prefix
        );
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
            info!(instance_id = instance_id, "No prior backup contents found; skipping auto-restore");
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

        info!(instance_id = instance_id, "Auto-restore completed successfully");
        Ok(())
    }

    /// Sync current VM state to B2 in destructive mode for teardown.
    ///
    /// This is used before destroy so files deleted on VM are deleted in B2.
    pub async fn sync_cleanup_on_destroy(
        context: &AppContext,
        remote: &RemoteExec,
        instance_id: u64,
        target_user: &str,
    ) -> AppResult<()> {
        let state = context.load_state().await;
        let settings = &state.shared_storage.settings;

        if !settings.enabled {
            info!(instance_id = instance_id, "Shared storage disabled; skipping destroy sync cleanup");
            return Ok(());
        }

        if settings.backblaze_key_id.trim().is_empty()
            || settings.backblaze_application_key.trim().is_empty()
        {
            info!(
                instance_id = instance_id,
                "Backblaze credentials missing; skipping destroy sync cleanup"
            );
            return Ok(());
        }

        Self::ensure_rclone_installed(remote).await?;
        Self::write_filter_rules(remote, target_user).await?;
        Self::configure_rclone_remote(remote, target_user, settings).await?;

        let effective_remote = Self::effective_remote_name(settings);
        let dest = format!(
            "{}:{}/{}",
            effective_remote, settings.bucket_name, settings.destination_prefix
        );
        let filter_path = format!("/home/{}/rules.txt", target_user);

        let sync_cmd = format!(
            "sudo -u {user} rclone sync / {dest} --filter-from {filter} --checksum --delete-excluded 2>&1",
            user = target_user,
            dest = shell_escape(&dest),
            filter = shell_escape(&filter_path),
        );

        let sync_output = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&sync_cmd, Duration::from_secs(3600)))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };

        if sync_output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Destroy sync cleanup failed (exit {}): {}",
                sync_output.status_code,
                redact_secrets(&sync_output.stdout, settings).trim()
            )));
        }

        // Post-check to ensure destination is reachable after sync.
        let verify_cmd = format!(
            "sudo -u {user} rclone lsf {dest} --max-depth 1 2>&1",
            user = target_user,
            dest = shell_escape(&dest),
        );
        let verify = {
            let remote = remote.clone();
            tokio::task::spawn_blocking(move || remote.ssh(&verify_cmd, Duration::from_secs(60)))
                .await
                .map_err(|e| AppError::Command(format!("join failure: {e}")))??
        };
        if verify.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "Destroy sync cleanup verification failed: {}",
                redact_secrets(&verify.stdout, settings).trim()
            )));
        }

        info!(instance_id = instance_id, "Destroy sync cleanup completed successfully");
        Ok(())
    }

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

/// Generate the crypt remote overlay name from the base remote name.
fn crypt_remote_name(base: &str) -> String {
    format!("{}-crypt", base)
}

/// Redact secrets from a string before logging.
fn redact_secrets(input: &str, settings: &crate::models::app_state::SharedStorageSettings) -> String {
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
