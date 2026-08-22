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
        Self::list_agent_apps(context, remote, target_user).await
    }
    pub async fn backup_selected_paths(
        context: &AppContext,
        remote: &RemoteExec,
        instance_id: u64,
        target_user: &str,
        selected_paths: &[String],
    ) -> AppResult<String> {
        let app_ids = selected_app_ids(selected_paths);
        let ids: Vec<String> = if app_ids.is_empty() {
            vec!["*".to_string()]
        } else {
            app_ids
        };
        Self::trigger_backup(context, remote, instance_id, target_user, &ids, "manual").await?;
        Ok("Application-state backup completed".to_string())
    }
    pub async fn list_remote_objects(
        context: &AppContext,
        remote: &RemoteExec,
        target_user: &str,
    ) -> AppResult<Vec<crate::models::app_state::SharedStorageObjectEntry>> {
        Self::catalog_to_entries(context, remote, target_user).await
    }
    pub async fn restore_selected_paths(
        context: &AppContext,
        remote: &RemoteExec,
        instance_id: u64,
        target_user: &str,
        selected_paths: &[String],
    ) -> AppResult<String> {
        let _ = instance_id;
        if selected_paths.is_empty() {
            return Err(AppError::InvalidInput(
                "Select at least one application bundle to restore.".into(),
            ));
        }
        let selections = selected_paths
            .iter()
            .map(|path| parse_catalog_selection(path))
            .collect::<AppResult<Vec<_>>>()?;

        let mut last = String::new();
        for (app_id, bundle_id) in selections {
            let result = Self::start_agent_restore(
                context,
                remote,
                target_user,
                &app_id,
                &bundle_id,
                "personal_state",
            )
            .await?;
            last = result.to_string();
        }
        Ok(format!("Application-state restore completed: {last}"))
    }
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
    pub async fn trigger_manual_backup(
        context: &AppContext,
        remote: &RemoteExec,
        instance_id: u64,
        target_user: &str,
    ) -> AppResult<()> {
        Self::trigger_backup(
            context,
            remote,
            instance_id,
            target_user,
            &["*".to_string()],
            "manual",
        )
        .await
    }
    pub(crate) async fn trigger_backup(
        context: &AppContext,
        remote: &RemoteExec,
        instance_id: u64,
        target_user: &str,
        app_ids: &[String],
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

        // Route the actual work through the live state-agent RPC path. An empty
        // list or "*" backs up every discovered app; otherwise each selected
        // app is backed up in turn. Errors are captured (not propagated with `?`)
        // so the running-job guard and state update below always run.
        let run_all = app_ids.is_empty() || app_ids.iter().any(|id| id == "*");
        let result: AppResult<()> = if run_all {
            Self::start_agent_backup(context, remote, target_user, "*", "personal_state", None)
                .await
                .map(|_| ())
        } else {
            let mut outcome: AppResult<()> = Ok(());
            for app_id in app_ids {
                if let Err(err) = Self::start_agent_backup(
                    context,
                    remote,
                    target_user,
                    app_id,
                    "personal_state",
                    None,
                )
                .await
                {
                    outcome = Err(err);
                    break;
                }
            }
            outcome
        };

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
        // Proactively refresh OAuth access tokens that are missing or about to
        // expire so downstream rclone sessions always start with a live token.
        let credentials =
            Self::refresh_oauth_if_needed(context, &profile, credentials, &provider_fields).await?;
        let remote_name = format!("noland_{}", profile.id.replace('-', ""));

        Ok(ActiveSharedStorageProfile {
            profile,
            credentials,
            provider_fields,
            remote_name,
        })
    }

    /// Refresh an OAuth2 access token proactively when it is missing or about
    /// to expire, persisting the rotated tokens back to the profile. Returns
    /// the (possibly refreshed) credentials so callers always mint sessions
    /// with a live token. Non-OAuth credentials are returned unchanged.
    async fn refresh_oauth_if_needed(
        context: &AppContext,
        profile: &SharedStorageProfile,
        credentials: StorageCredential,
        provider_fields: &HashMap<String, String>,
    ) -> AppResult<StorageCredential> {
        let StorageCredential::OAuth2 {
            access_token,
            refresh_token,
            expires_at,
        } = &credentials
        else {
            return Ok(credentials);
        };

        // Refresh when the access token is empty, the expiry is unset, or the
        // token is within the refresh skew of expiring.
        const REFRESH_SKEW_SECS: i64 = 60;
        let now = chrono::Utc::now().timestamp();
        let needs_refresh = access_token.trim().is_empty()
            || *expires_at <= 0
            || *expires_at - now < REFRESH_SKEW_SECS;

        let Some(refresh) = refresh_token.as_ref().filter(|v| !v.trim().is_empty()) else {
            if needs_refresh {
                warn!(
                    "OAuth token for '{}' is expired but no refresh token is stored; re-authentication required",
                    profile.display_name
                );
            }
            return Ok(credentials);
        };

        if !needs_refresh {
            return Ok(credentials);
        }

        let client_id = provider_fields
            .get("client_id")
            .map(String::as_str)
            .unwrap_or("");
        let client_secret = provider_fields.get("client_secret").map(String::as_str);
        let Some(oauth_config) =
            super::oauth_flow::get_oauth_config(&profile.provider, client_id, client_secret)
        else {
            warn!(
                "OAuth token for '{}' expired but no OAuth config is available for provider {:?}; re-authentication required",
                profile.display_name, profile.provider
            );
            return Ok(credentials);
        };

        info!(
            "Refreshing OAuth access token for '{}' (provider {:?})",
            profile.display_name, profile.provider
        );
        match super::oauth_flow::refresh_token(&oauth_config, refresh).await {
            Ok(token) => {
                let new_expires_at = now + token.expires_in.unwrap_or(3600);
                // Some providers rotate the refresh token; keep the new one
                // when present and fall back to the existing token otherwise.
                let new_refresh = token
                    .refresh_token
                    .clone()
                    .filter(|v| !v.trim().is_empty())
                    .or_else(|| refresh_token.clone());

                let refreshed = StorageCredential::OAuth2 {
                    access_token: token.access_token.clone(),
                    refresh_token: new_refresh,
                    expires_at: new_expires_at,
                };

                let profile_id = profile.id.clone();
                let persisted = refreshed.clone();
                context
                    .update_state(move |state| {
                        if let Some(stored) = state
                            .shared_storage_credentials
                            .profiles
                            .get_mut(&profile_id)
                        {
                            stored.credentials = persisted.clone();
                        }
                    })
                    .await?;

                info!(
                    "OAuth token refreshed for '{}' ({}), new expiry in {}s",
                    profile.display_name,
                    profile.id,
                    new_expires_at - now
                );
                Ok(refreshed)
            }
            Err(e) => {
                warn!(
                    "OAuth token refresh failed for '{}': {e}. Re-authentication may be required.",
                    profile.display_name
                );
                // Return the stale credential so rclone can attempt the call
                // and surface the provider's specific auth error.
                Ok(credentials)
            }
        }
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
        super::rclone_adapter::mint_config_ini(
            &active_profile.profile.provider,
            &active_profile.credentials,
            &active_profile.provider_fields,
            active_profile.profile.bucket.as_deref(),
            active_profile.profile.prefix.as_deref(),
            &active_profile.remote_name,
            noland_rclone_adapter::TokenMode::Durable,
        )
    }

    pub(crate) fn build_profile_storage_source(
        active_profile: &ActiveSharedStorageProfile,
    ) -> String {
        super::rclone_adapter::storage_source(
            &active_profile.profile.provider,
            &active_profile.credentials,
            &active_profile.provider_fields,
            active_profile.profile.bucket.as_deref(),
            active_profile.profile.prefix.as_deref(),
            &active_profile.remote_name,
        )
        .unwrap_or_else(|_| format!("{}:", active_profile.remote_name))
    }

    pub fn mint_ephemeral_rclone_session(
        active_profile: &ActiveSharedStorageProfile,
        operation_id: &str,
    ) -> AppResult<noland_rclone_adapter::EphemeralRcloneSession> {
        super::rclone_adapter::mint_ephemeral_session(
            &active_profile.profile.provider,
            &active_profile.credentials,
            &active_profile.provider_fields,
            active_profile.profile.bucket.as_deref(),
            active_profile.profile.prefix.as_deref(),
            &active_profile.remote_name,
            operation_id,
        )
    }

    /// Ensure rclone is installed on the VM.
    pub(crate) async fn ensure_rclone_installed(remote: &RemoteExec) -> AppResult<()> {
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

fn selected_app_ids(paths: &[String]) -> Vec<String> {
    let mut ids = Vec::new();
    for path in paths {
        if let Some(rest) = path.strip_prefix("/apps/") {
            let id = rest.trim_matches('/');
            if !id.is_empty() && id != "Applications" {
                ids.push(id.to_string());
            }
        }
    }
    ids
}

fn parse_catalog_selection(path: &str) -> crate::errors::AppResult<(String, String)> {
    let rest = path.strip_prefix("/catalog/").ok_or_else(|| {
        crate::errors::AppError::InvalidInput(format!(
            "Restore selection '{path}' is not a catalog application bundle. Expand an application and choose a specific backup bundle."
        ))
    })?;
    let mut parts = rest.split('/');
    let app_id = parts.next().unwrap_or_default();
    let bundle_id = parts.next().filter(|value| !value.is_empty()).ok_or_else(|| {
        crate::errors::AppError::InvalidInput(
            "An application folder cannot be restored directly. Expand it and choose a specific backup bundle."
                .into(),
        )
    })?;

    if app_id.is_empty() {
        return Err(crate::errors::AppError::InvalidInput(
            "Catalog application ID is missing. Reload the Sync catalog and try again.".into(),
        ));
    }
    if parts.next().is_some() {
        return Err(crate::errors::AppError::InvalidInput(format!(
            "Restore selection '{path}' contains an unexpected nested path. Reload the Sync catalog and choose a bundle."
        )));
    }
    uuid::Uuid::parse_str(bundle_id).map_err(|_| {
        crate::errors::AppError::InvalidInput(format!(
            "Backup bundle '{bundle_id}' has an invalid ID. Reload the Sync catalog or create a new backup."
        ))
    })?;

    Ok((app_id.to_string(), bundle_id.to_string()))
}

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

#[cfg(test)]
mod tests {
    use super::parse_catalog_selection;

    #[test]
    fn parses_specific_catalog_bundle_selection() {
        let bundle_id = "15c739e7-6fd8-4bd8-b8b6-beb335156c76";
        let parsed = parse_catalog_selection(&format!("/catalog/steam:480/{bundle_id}"))
            .expect("specific bundle selection should be valid");

        assert_eq!(parsed, ("steam:480".to_string(), bundle_id.to_string()));
    }

    #[test]
    fn rejects_catalog_application_folder_selection() {
        let error = parse_catalog_selection("/catalog/steam:480")
            .expect_err("application folder should not be accepted as a bundle");

        assert!(error.to_string().contains("application folder"));
    }

    #[test]
    fn rejects_non_uuid_catalog_bundle_selection() {
        let error = parse_catalog_selection("/catalog/steam:480/latest")
            .expect_err("non-UUID bundle IDs should be rejected");

        assert!(error.to_string().contains("invalid ID"));
    }
}
