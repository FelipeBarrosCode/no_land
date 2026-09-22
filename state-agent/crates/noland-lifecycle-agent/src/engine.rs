use std::collections::HashSet;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use tracing::{info, warn};
use uuid::Uuid;

use crate::activity::ActivityProcessor;
use crate::capability::StorageCapability;
use crate::clock::Clock;
use crate::config::Config;
use crate::db::Database;
use crate::model::{BackupApp, LifecycleState, RankedApp};
use crate::provider::{ProviderLifecycle, ProviderRequest};
use crate::state_agent::{
    BackupRequest, ForegroundPidBackend, OperationStatus, StateAgentClient, VerifyRequest,
};
use crate::{AgentError, Result};

const BACKUP_POLL_INTERVAL: Duration = Duration::from_secs(2);
const BACKUP_POLL_TIMEOUT_MS: u64 = 30 * 60 * 1_000;
const BACKUP_MAX_ATTEMPTS: u32 = 3;
const MAX_TRACKED_APPS: usize = 1_024;
const BACKOFFS: [Duration; 3] = [
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(30),
];

#[async_trait]
pub trait Sleeper: Send + Sync {
    async fn sleep(&self, duration: Duration);
}

pub struct TokioSleeper;

#[async_trait]
impl Sleeper for TokioSleeper {
    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

pub trait CapabilitySource: Send + Sync {
    fn load(&self, config: &Config, now: DateTime<Utc>) -> Result<StorageCapability>;
}

pub struct FileCapabilitySource;

impl CapabilitySource for FileCapabilitySource {
    fn load(&self, config: &Config, now: DateTime<Utc>) -> Result<StorageCapability> {
        StorageCapability::load_and_validate(&config.capability_path, config, now)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStatus {
    pub instance_id: u64,
    pub enabled: bool,
    pub state: LifecycleState,
    pub last_activity_at: Option<DateTime<Utc>>,
    pub idle_duration_ms: u64,
    pub timeout_ms: u64,
    pub time_remaining_ms: u64,
    pub timeout_reached_at: Option<DateTime<Utc>>,
    pub active_run_id: Option<String>,
    pub ranked_apps: Vec<RankedApp>,
    pub last_error: Option<String>,
}

pub struct LifecycleEngine {
    pub config: Arc<RwLock<Config>>,
    database: Arc<Database>,
    activity: Arc<ActivityProcessor>,
    state_agent: Arc<dyn StateAgentClient>,
    foreground: Arc<dyn ForegroundPidBackend>,
    provider: Arc<dyn ProviderLifecycle>,
    capability_source: Arc<dyn CapabilitySource>,
    clock: Arc<dyn Clock>,
    sleeper: Arc<dyn Sleeper>,
    last_usage_tick_ms: Mutex<u64>,
    candidate_app_ids: RwLock<HashSet<String>>,
}

impl LifecycleEngine {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: Arc<RwLock<Config>>,
        database: Arc<Database>,
        activity: Arc<ActivityProcessor>,
        state_agent: Arc<dyn StateAgentClient>,
        foreground: Arc<dyn ForegroundPidBackend>,
        provider: Arc<dyn ProviderLifecycle>,
        capability_source: Arc<dyn CapabilitySource>,
        clock: Arc<dyn Clock>,
        sleeper: Arc<dyn Sleeper>,
    ) -> Result<Self> {
        let now_mono = clock.monotonic_ms();
        if database.snapshot()?.state.run_is_active() {
            activity.latch_timeout();
        }
        Ok(Self {
            config,
            database,
            activity,
            state_agent,
            foreground,
            provider,
            capability_source,
            clock,
            sleeper,
            last_usage_tick_ms: Mutex::new(now_mono),
            candidate_app_ids: RwLock::new(HashSet::new()),
        })
    }

    pub fn status(&self) -> Result<AgentStatus> {
        let config = self
            .config
            .read()
            .map_err(|_| AgentError::new("configuration lock poisoned"))?
            .clone();
        let snapshot = self.database.snapshot()?;
        let idle = self.activity.idle_duration_ms();
        let timeout = config.timeout_ms();
        Ok(AgentStatus {
            instance_id: config.instance_id,
            enabled: config.enabled,
            state: snapshot.state,
            last_activity_at: snapshot.last_activity_at,
            idle_duration_ms: idle,
            timeout_ms: timeout,
            time_remaining_ms: if self.activity.timeout_is_latched() {
                0
            } else {
                timeout.saturating_sub(idle)
            },
            timeout_reached_at: snapshot.timeout_reached_at,
            active_run_id: snapshot.active_run_id,
            ranked_apps: self.eligible_ranked_apps(config.backup_app_limit)?,
            last_error: snapshot.last_error,
        })
    }

    pub fn reload_config(&self, path: &std::path::Path) -> Result<()> {
        let new_config = Config::load(path)?;
        let mut current = self
            .config
            .write()
            .map_err(|_| AgentError::new("configuration lock poisoned"))?;
        if !current.runtime_paths_match(&new_config) {
            return Err(AgentError::new(
                "socket and database path changes require an agent restart",
            ));
        }
        let newly_enabled = !current.enabled && new_config.enabled;
        *current = new_config;
        let enabled = current.enabled;
        drop(current);
        if enabled {
            // Saving enabled lifecycle settings is also the explicit user
            // action to re-arm a previous safe failure. Only terminal safety
            // states are reset; active backup/shutdown runs remain intact.
            self.database.reset_terminal_failure(self.clock.now_utc())?;
            if newly_enabled {
                self.database
                    .initialize_monitoring(true, self.clock.now_utc())?;
                self.activity.reset_for_monitoring()?;
            }
        } else if !enabled {
            let snapshot = self.database.snapshot()?;
            if !snapshot.state.run_is_active() {
                self.database
                    .initialize_monitoring(false, self.clock.now_utc())?;
            }
        }
        Ok(())
    }

    pub async fn tick(&self) -> Result<()> {
        let config = self
            .config
            .read()
            .map_err(|_| AgentError::new("configuration lock poisoned"))?
            .clone();
        if !config.enabled {
            let snapshot = self.database.snapshot()?;
            if !snapshot.state.run_is_active() && snapshot.state != LifecycleState::Disabled {
                self.database
                    .initialize_monitoring(false, self.clock.now_utc())?;
            }
            return Ok(());
        }

        if let Err(error) = self.update_usage().await {
            warn!(error = %error, "usage sampling failed");
        }

        let snapshot = self.database.snapshot()?;
        if snapshot.state.run_is_active() {
            let run_id = snapshot
                .active_run_id
                .ok_or_else(|| AgentError::new("active lifecycle state has no run ID"))?;
            return self.execute_run(&run_id, &config).await;
        }
        if matches!(
            snapshot.state,
            LifecycleState::BackupFailedSafe
                | LifecycleState::ShutdownFailedSafe
                | LifecycleState::Completed
        ) {
            return Ok(());
        }

        if !self
            .activity
            .latch_timeout_if_elapsed(config.timeout_ms())?
        {
            if snapshot.state != LifecycleState::MonitoringIdle {
                self.database.transition(
                    LifecycleState::MonitoringIdle,
                    self.clock.now_utc(),
                    None,
                )?;
            }
            return Ok(());
        }

        let timeout_at = self.clock.now_utc();
        self.database
            .transition(LifecycleState::TimeoutReached, timeout_at, None)?;
        self.database.transition(
            LifecycleState::SelectingApplications,
            self.clock.now_utc(),
            None,
        )?;
        let apps = match self.validated_ranked_apps(config.backup_app_limit).await {
            Ok(apps) => apps,
            Err(error) => {
                self.database.fail_run(
                    None,
                    LifecycleState::BackupFailedSafe,
                    &format!(
                        "unable to validate automatic-backup applications against shared storage: {}",
                        error.public_message()
                    ),
                )?;
                return Ok(());
            }
        };
        if apps.is_empty() {
            self.database.fail_run(
                None,
                LifecycleState::BackupFailedSafe,
                "no eligible ranked applications; provider action blocked",
            )?;
            return Ok(());
        }
        let run_id = self
            .database
            .create_run_with_frozen_apps(timeout_at, &apps)?;
        self.execute_run(&run_id, &config).await
    }

    async fn update_usage(&self) -> Result<()> {
        let now_mono = self.clock.monotonic_ms();
        let elapsed = {
            let mut previous = self
                .last_usage_tick_ms
                .lock()
                .map_err(|_| AgentError::new("usage tick lock poisoned"))?;
            let elapsed = now_mono.saturating_sub(*previous);
            *previous = now_mono;
            elapsed
        };
        if elapsed == 0 {
            return Ok(());
        }
        let sessions = self.state_agent.get_active_app_sessions().await?;
        let candidate_app_ids = self.state_agent.list_backup_candidate_app_ids().await?;
        {
            let mut cached = self
                .candidate_app_ids
                .write()
                .map_err(|_| AgentError::new("backup candidate cache lock poisoned"))?;
            *cached = candidate_app_ids.clone();
        }
        let now = self.clock.now_utc();
        for session in sessions {
            if candidate_app_ids.contains(&session.app_id)
                && is_backup_eligible_app_id(&session.app_id)
            {
                self.database.record_active_session(
                    &session.session_id,
                    &session.app_id,
                    elapsed,
                    now,
                )?;
            }
        }
        if let Some(pid) = self.foreground.foreground_pid().await {
            if let Some(app_id) = self.state_agent.resolve_process_to_app(pid).await? {
                if candidate_app_ids.contains(&app_id) && is_backup_eligible_app_id(&app_id) {
                    self.database.record_foreground(&app_id, elapsed, now)?;
                }
            }
        }
        Ok(())
    }

    fn eligible_ranked_apps(&self, limit: usize) -> Result<Vec<RankedApp>> {
        let candidate_app_ids = self
            .candidate_app_ids
            .read()
            .map_err(|_| AgentError::new("backup candidate cache lock poisoned"))?;
        Ok(self
            .database
            .ranked_apps(MAX_TRACKED_APPS)?
            .into_iter()
            .filter(|app| {
                candidate_app_ids.contains(&app.app_id) && is_backup_eligible_app_id(&app.app_id)
            })
            .take(limit)
            .collect())
    }

    async fn validated_ranked_apps(&self, limit: usize) -> Result<Vec<RankedApp>> {
        let candidate_app_ids = self.state_agent.list_backup_candidate_app_ids().await?;
        {
            let mut cached = self
                .candidate_app_ids
                .write()
                .map_err(|_| AgentError::new("backup candidate cache lock poisoned"))?;
            *cached = candidate_app_ids;
        }
        self.eligible_ranked_apps(limit)
    }

    async fn execute_run(&self, run_id: &str, config: &Config) -> Result<()> {
        let capability = match self.capability_source.load(config, self.clock.now_utc()) {
            Ok(capability) => capability,
            Err(error) => {
                self.database.fail_run(
                    Some(run_id),
                    LifecycleState::BackupFailedSafe,
                    error.public_message(),
                )?;
                return Ok(());
            }
        };

        for app in self.database.backup_apps(run_id)? {
            if app.status == "VERIFIED" {
                continue;
            }
            if let Err(error) = self.backup_app(run_id, app, &capability).await {
                self.database.fail_run(
                    Some(run_id),
                    LifecycleState::BackupFailedSafe,
                    error.public_message(),
                )?;
                return Ok(());
            }
        }

        if !self.database.all_apps_verified(run_id)? {
            self.database.fail_run(
                Some(run_id),
                LifecycleState::BackupFailedSafe,
                "not every selected application has a verified backup",
            )?;
            return Ok(());
        }

        let mut last_error = None;
        for provider_attempt in 0..=BACKOFFS.len() {
            if provider_attempt > 0 {
                self.database.transition(
                    LifecycleState::ShutdownRetryWait,
                    self.clock.now_utc(),
                    last_error.as_deref(),
                )?;
                self.sleeper.sleep(BACKOFFS[provider_attempt - 1]).await;
            }
            if let Err(error) = capability.validate(config, self.clock.now_utc()) {
                self.database.fail_run(
                    Some(run_id),
                    LifecycleState::ShutdownFailedSafe,
                    error.public_message(),
                )?;
                return Ok(());
            }
            self.database
                .mark_shutdown_started(run_id, self.clock.now_utc())?;
            let request = ProviderRequest {
                base_url: config.vast_base_url.clone(),
                instance_id: config.instance_id,
                action: config.provider_action,
                api_key: capability.provider.api_key.clone(),
            };
            match self.provider.apply(request).await {
                Ok(()) => {
                    self.database.complete_run(run_id, self.clock.now_utc())?;
                    info!(run_id, "lifecycle provider action completed");
                    return Ok(());
                }
                Err(error) => last_error = Some(error.to_string()),
            }
        }
        self.database.fail_run(
            Some(run_id),
            LifecycleState::ShutdownFailedSafe,
            last_error.as_deref().unwrap_or("provider action failed"),
        )?;
        Ok(())
    }

    async fn backup_app(
        &self,
        run_id: &str,
        mut app: BackupApp,
        capability: &StorageCapability,
    ) -> Result<()> {
        if app.status == "RUNNING" {
            if let Some(operation_id) = app.operation_id.as_deref() {
                match self
                    .monitor_and_verify(&app.app_id, operation_id, capability)
                    .await
                {
                    Ok((bundle_id, commit_id)) => {
                        self.database.mark_app_verified(
                            run_id,
                            &app.app_id,
                            &bundle_id,
                            &commit_id,
                            self.clock.now_utc(),
                        )?;
                        return Ok(());
                    }
                    Err(error) => {
                        self.database.mark_app_pending_retry(
                            run_id,
                            &app.app_id,
                            error.public_message(),
                            self.clock.now_utc(),
                        )?;
                    }
                }
            } else {
                self.database.mark_app_pending_retry(
                    run_id,
                    &app.app_id,
                    "persisted running backup has no operation ID",
                    self.clock.now_utc(),
                )?;
            }
            app = self
                .database
                .backup_apps(run_id)?
                .into_iter()
                .find(|candidate| candidate.app_id == app.app_id)
                .ok_or_else(|| AgentError::new("frozen backup application disappeared"))?;
        }

        while app.attempts < BACKUP_MAX_ATTEMPTS {
            if app.attempts > 0 {
                self.sleeper
                    .sleep(BACKOFFS[(app.attempts - 1) as usize])
                    .await;
            }
            let requested_operation_id = Uuid::new_v4();
            let request = BackupRequest {
                app_id: app.app_id.clone(),
                operation_id: requested_operation_id,
                session: capability.session_for_operation(requested_operation_id),
                master_key_hex: capability.master_key_hex.clone(),
            };
            let operation_id = match self.state_agent.start_backup(request).await {
                Ok(operation_id) if !operation_id.trim().is_empty() => operation_id,
                Ok(_) => {
                    app.attempts += 1;
                    self.database.record_start_failure(
                        run_id,
                        &app.app_id,
                        "StartBackup returned an empty operation ID",
                        self.clock.now_utc(),
                    )?;
                    if app.attempts >= BACKUP_MAX_ATTEMPTS {
                        self.database.mark_app_failed(
                            run_id,
                            &app.app_id,
                            "StartBackup returned an empty operation ID",
                            self.clock.now_utc(),
                        )?;
                        return Err(AgentError::new(
                            "StartBackup returned an empty operation ID",
                        ));
                    }
                    continue;
                }
                Err(error) => {
                    app.attempts += 1;
                    self.database.record_start_failure(
                        run_id,
                        &app.app_id,
                        error.public_message(),
                        self.clock.now_utc(),
                    )?;
                    if app.attempts >= BACKUP_MAX_ATTEMPTS {
                        self.database.mark_app_failed(
                            run_id,
                            &app.app_id,
                            error.public_message(),
                            self.clock.now_utc(),
                        )?;
                        return Err(error);
                    }
                    continue;
                }
            };
            self.database.mark_app_running(
                run_id,
                &app.app_id,
                &operation_id,
                self.clock.now_utc(),
            )?;
            app.attempts += 1;
            match self
                .monitor_and_verify(&app.app_id, &operation_id, capability)
                .await
            {
                Ok((bundle_id, commit_id)) => {
                    self.database.mark_app_verified(
                        run_id,
                        &app.app_id,
                        &bundle_id,
                        &commit_id,
                        self.clock.now_utc(),
                    )?;
                    return Ok(());
                }
                Err(error) if app.attempts < BACKUP_MAX_ATTEMPTS => {
                    self.database.mark_app_pending_retry(
                        run_id,
                        &app.app_id,
                        error.public_message(),
                        self.clock.now_utc(),
                    )?;
                }
                Err(error) => {
                    self.database.mark_app_failed(
                        run_id,
                        &app.app_id,
                        error.public_message(),
                        self.clock.now_utc(),
                    )?;
                    return Err(error);
                }
            }
        }
        Err(AgentError::new("backup attempts exhausted"))
    }

    async fn monitor_and_verify(
        &self,
        app_id: &str,
        operation_id: &str,
        capability: &StorageCapability,
    ) -> Result<(String, String)> {
        let started = self.clock.monotonic_ms();
        loop {
            let status = self.state_agent.get_operation_status(operation_id).await?;
            match status.state.as_str() {
                "COMPLETED" => {
                    let (bundle_id, commit_id) = completion_ids(&status)?;
                    self.database.transition(
                        LifecycleState::Verifying,
                        self.clock.now_utc(),
                        None,
                    )?;
                    let verify_operation_id = Uuid::new_v4();
                    let verified = self
                        .state_agent
                        .verify_backup_commit(VerifyRequest {
                            app_id: app_id.to_string(),
                            bundle_id: bundle_id.clone(),
                            commit_id: commit_id.clone(),
                            session: capability.session_for_operation(verify_operation_id),
                            master_key_hex: capability.master_key_hex.clone(),
                        })
                        .await?;
                    if !verified {
                        return Err(AgentError::new("backup commit verification failed"));
                    }
                    self.database.transition(
                        LifecycleState::BackingUp,
                        self.clock.now_utc(),
                        None,
                    )?;
                    return Ok((bundle_id, commit_id));
                }
                "FAILED" | "CANCELLED" | "INTERRUPTED" => {
                    let reason = status
                        .detail_json
                        .get("_last_error")
                        .or_else(|| status.detail_json.get("last_error"))
                        .or_else(|| status.detail_json.get("error"))
                        .and_then(Value::as_str)
                        .filter(|error| !error.trim().is_empty());
                    return Err(AgentError::new(match reason {
                        Some(reason) => format!(
                            "backup operation ended in unsafe state {}: {}",
                            status.state, reason
                        ),
                        None => format!("backup operation ended in unsafe state {}", status.state),
                    }));
                }
                "QUEUED" | "DISCOVERING" | "RECONCILING" | "SNAPSHOTTING" | "HASHING"
                | "PACKING" | "UPLOADING" | "COMMITTING" | "CHECKPOINTING" | "RUNNING" => {}
                _ => {
                    return Err(AgentError::new(format!(
                        "backup operation returned unknown state {}",
                        status.state
                    )))
                }
            }
            if self.clock.monotonic_ms().saturating_sub(started) >= BACKUP_POLL_TIMEOUT_MS {
                return Err(AgentError::new("backup operation timed out"));
            }
            self.sleeper.sleep(BACKUP_POLL_INTERVAL).await;
        }
    }
}

fn completion_ids(status: &OperationStatus) -> Result<(String, String)> {
    fn string_field(value: &Value, snake: &str, camel: &str) -> Option<String> {
        value
            .get(snake)
            .or_else(|| value.get(camel))
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
    }
    let bundle_id = string_field(&status.detail_json, "bundle_id", "bundleId")
        .ok_or_else(|| AgentError::new("completed backup has no bundle ID"))?;
    let commit_id = string_field(&status.detail_json, "commit_id", "commitId")
        .ok_or_else(|| AgentError::new("completed backup has no commit ID"))?;
    Ok((bundle_id, commit_id))
}

fn is_backup_eligible_app_id(app_id: &str) -> bool {
    let normalized = app_id.trim().to_ascii_lowercase();
    let Some(executable_id) = normalized.strip_prefix("exe:") else {
        return true;
    };
    let executable = executable_id
        .rsplit_once(':')
        .map_or(executable_id, |(name, _)| name)
        .rsplit('/')
        .next()
        .unwrap_or(executable_id);

    !noland_discovery::is_always_ignored_executable_name(executable)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use chrono::TimeZone;
    use noland_rclone_adapter::{
        session_from_input, AdapterCredential, AdapterInput, ProviderKind, TokenMode,
    };
    use serde_json::json;

    use super::*;

    #[test]
    fn backup_eligibility_excludes_system_daemons_but_keeps_user_apps() {
        for ignored in [
            "exe:upowerd:25980-6228f901",
            "exe:systemd:1-session",
            "exe:xdg-desktop-portal-kde:42-session",
            "exe:/usr/libexec/polkitd:71-session",
            "exe:startplasma-x11:6228f901",
            "exe:/usr/bin/plasmashell:71-session",
        ] {
            assert!(!is_backup_eligible_app_id(ignored), "{ignored}");
        }

        for eligible in [
            "exe:hydralauncher:155766-session",
            "exe:/tmp/.mount_hydra/hydralauncher:155766-session",
            "desktop:hydralauncher",
            "steam:12345",
        ] {
            assert!(is_backup_eligible_app_id(eligible), "{eligible}");
        }
    }
    use crate::activity::ActivityRecorder;
    use crate::capability::{VastCapability, VastProviderKind};
    use crate::clock::ManualClock;
    use crate::config::ProviderAction;
    use crate::state_agent::{ActiveAppSession, OperationStatus};

    struct NoSleep;
    #[async_trait]
    impl Sleeper for NoSleep {
        async fn sleep(&self, _duration: Duration) {}
    }

    struct ExpiringSleep(ManualClock);
    #[async_trait]
    impl Sleeper for ExpiringSleep {
        async fn sleep(&self, _duration: Duration) {
            self.0.advance(Duration::from_secs(100_000));
        }
    }

    struct NoForeground;
    #[async_trait]
    impl ForegroundPidBackend for NoForeground {
        async fn foreground_pid(&self) -> Option<u32> {
            None
        }
    }

    struct FixedCapability(StorageCapability);
    impl CapabilitySource for FixedCapability {
        fn load(&self, config: &Config, now: DateTime<Utc>) -> Result<StorageCapability> {
            self.0.validate(config, now)?;
            Ok(self.0.clone())
        }
    }

    struct FakeProvider {
        calls: AtomicUsize,
        failures: AtomicUsize,
    }
    #[async_trait]
    impl ProviderLifecycle for FakeProvider {
        async fn apply(&self, _request: ProviderRequest) -> Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.failures.load(Ordering::SeqCst) > 0 {
                self.failures.fetch_sub(1, Ordering::SeqCst);
                Err(AgentError::new("provider unavailable"))
            } else {
                Ok(())
            }
        }
    }

    struct FakeStateAgent {
        statuses: Mutex<VecDeque<OperationStatus>>,
        verify: bool,
        starts: AtomicUsize,
        start_error: Mutex<Option<String>>,
    }
    #[async_trait]
    impl StateAgentClient for FakeStateAgent {
        async fn get_active_app_sessions(&self) -> Result<Vec<ActiveAppSession>> {
            Ok(Vec::new())
        }
        async fn list_backup_candidate_app_ids(&self) -> Result<HashSet<String>> {
            Ok(["app", "one", "two"]
                .into_iter()
                .map(str::to_owned)
                .collect())
        }
        async fn resolve_process_to_app(&self, _pid: u32) -> Result<Option<String>> {
            Ok(None)
        }
        async fn start_backup(&self, _request: BackupRequest) -> Result<String> {
            if let Some(error) = self.start_error.lock().unwrap().clone() {
                return Err(AgentError::new(error));
            }
            let value = self.starts.fetch_add(1, Ordering::SeqCst);
            Ok(format!("operation-{value}"))
        }
        async fn get_operation_status(&self, _operation_id: &str) -> Result<OperationStatus> {
            self.statuses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| AgentError::new("no fake status"))
        }
        async fn verify_backup_commit(&self, _request: VerifyRequest) -> Result<bool> {
            Ok(self.verify)
        }
    }

    fn completed() -> OperationStatus {
        OperationStatus {
            state: "COMPLETED".into(),
            detail_json: json!({"bundle_id": Uuid::new_v4(), "commit_id": Uuid::new_v4()}),
        }
    }

    fn capability() -> StorageCapability {
        let session = session_from_input(
            &AdapterInput {
                provider: ProviderKind::Local,
                remote_name: "test".into(),
                credentials: AdapterCredential::LocalPath {
                    path: "/tmp".into(),
                },
                fields: BTreeMap::new(),
                bucket: None,
                prefix: None,
            },
            "seed",
            TokenMode::Operation,
        )
        .unwrap();
        StorageCapability {
            version: 1,
            instance_id: 42,
            profile_id: "profile".into(),
            expires_at: Utc.timestamp_opt(100_000, 0).unwrap(),
            session,
            master_key_hex: "ab".repeat(32),
            provider: VastCapability {
                kind: VastProviderKind::Vast,
                api_key: "secret".into(),
                instance_id: 42,
                action: ProviderAction::Destroy,
            },
        }
    }

    struct Harness {
        engine: LifecycleEngine,
        database: Arc<Database>,
        activity: Arc<ActivityProcessor>,
        clock: ManualClock,
        provider: Arc<FakeProvider>,
        state_agent: Arc<FakeStateAgent>,
    }

    fn harness(statuses: Vec<OperationStatus>, verify: bool) -> Harness {
        harness_with_expiring_retry(statuses, verify, false)
    }

    fn harness_with_expiring_retry(
        statuses: Vec<OperationStatus>,
        verify: bool,
        expire_on_retry: bool,
    ) -> Harness {
        let config = Arc::new(RwLock::new(Config {
            enabled: true,
            instance_id: 42,
            inactivity_seconds: 900,
            ..Config::default()
        }));
        let database = Arc::new(Database::open_in_memory().unwrap());
        let clock = ManualClock::new(Utc.timestamp_opt(1_000, 0).unwrap());
        database
            .initialize_monitoring(true, clock.now_utc())
            .unwrap();
        let activity = Arc::new(ActivityProcessor::new(
            Arc::clone(&database),
            Arc::clone(&config),
            Arc::new(clock.clone()),
        ));
        let provider = Arc::new(FakeProvider {
            calls: AtomicUsize::new(0),
            failures: AtomicUsize::new(0),
        });
        let state_agent = Arc::new(FakeStateAgent {
            statuses: Mutex::new(statuses.into()),
            verify,
            starts: AtomicUsize::new(0),
            start_error: Mutex::new(None),
        });
        let sleeper: Arc<dyn Sleeper> = if expire_on_retry {
            Arc::new(ExpiringSleep(clock.clone()))
        } else {
            Arc::new(NoSleep)
        };
        let engine = LifecycleEngine::new(
            config,
            Arc::clone(&database),
            Arc::clone(&activity),
            state_agent.clone(),
            Arc::new(NoForeground),
            provider.clone(),
            Arc::new(FixedCapability(capability())),
            Arc::new(clock.clone()),
            sleeper,
        )
        .unwrap();
        Harness {
            engine,
            database,
            activity,
            clock,
            provider,
            state_agent,
        }
    }

    fn add_usage(database: &Database, clock: &ManualClock, app_id: &str, foreground: u64) {
        database
            .record_active_session(&format!("session-{app_id}"), app_id, 1_000, clock.now_utc())
            .unwrap();
        database
            .record_foreground(app_id, foreground, clock.now_utc())
            .unwrap();
    }

    #[test]
    fn ranking_only_contains_shared_storage_candidates() {
        let harness = harness(vec![], true);
        add_usage(&harness.database, &harness.clock, "stale-unindexed", 3_000);
        add_usage(&harness.database, &harness.clock, "one", 2_000);
        add_usage(&harness.database, &harness.clock, "two", 1_000);
        *harness.engine.candidate_app_ids.write().unwrap() =
            ["one", "two"].into_iter().map(str::to_owned).collect();

        let ranked = harness.engine.eligible_ranked_apps(2).unwrap();
        assert_eq!(
            ranked.into_iter().map(|app| app.app_id).collect::<Vec<_>>(),
            vec!["one", "two"]
        );
    }

    #[tokio::test]
    async fn no_action_before_timeout() {
        let harness = harness(vec![], true);
        add_usage(&harness.database, &harness.clock, "app", 1_000);
        harness.clock.advance(Duration::from_secs(899));
        harness.engine.tick().await.unwrap();
        assert_eq!(harness.provider.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            harness.database.snapshot().unwrap().state,
            LifecycleState::MonitoringIdle
        );
    }

    #[tokio::test]
    async fn successful_verified_apps_permit_provider() {
        let harness = harness(vec![completed(), completed()], true);
        add_usage(&harness.database, &harness.clock, "one", 2_000);
        add_usage(&harness.database, &harness.clock, "two", 1_000);
        harness.clock.advance(Duration::from_secs(900));
        harness.engine.tick().await.unwrap();
        assert_eq!(harness.provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            harness.database.snapshot().unwrap().state,
            LifecycleState::Completed
        );
    }

    #[tokio::test]
    async fn expired_capability_blocks_provider_retry() {
        let harness = harness_with_expiring_retry(vec![completed()], true, true);
        harness.provider.failures.store(1, Ordering::SeqCst);
        add_usage(&harness.database, &harness.clock, "app", 1_000);
        harness.clock.advance(Duration::from_secs(900));
        harness.engine.tick().await.unwrap();
        assert_eq!(harness.provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            harness.database.snapshot().unwrap().state,
            LifecycleState::ShutdownFailedSafe
        );
    }

    #[tokio::test]
    async fn backup_partial_failure_blocks_provider() {
        let failed = OperationStatus {
            state: "FAILED".into(),
            detail_json: json!({}),
        };
        let harness = harness(
            vec![completed(), failed.clone(), failed.clone(), failed],
            true,
        );
        add_usage(&harness.database, &harness.clock, "one", 2_000);
        add_usage(&harness.database, &harness.clock, "two", 1_000);
        harness.clock.advance(Duration::from_secs(900));
        harness.engine.tick().await.unwrap();
        assert_eq!(harness.provider.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            harness.database.snapshot().unwrap().state,
            LifecycleState::BackupFailedSafe
        );
    }

    #[tokio::test]
    async fn missing_frozen_app_blocks_provider() {
        let harness = harness(vec![], true);
        *harness.state_agent.start_error.lock().unwrap() =
            Some("not found: app was removed from the shared-storage index".into());
        add_usage(&harness.database, &harness.clock, "app", 1_000);
        harness.clock.advance(Duration::from_secs(900));

        harness.engine.tick().await.unwrap();

        assert_eq!(harness.provider.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            harness.database.snapshot().unwrap().state,
            LifecycleState::BackupFailedSafe
        );
    }

    #[tokio::test]
    async fn restart_unknown_operation_fails_safe() {
        let harness = harness(
            vec![
                OperationStatus {
                    state: "MYSTERY".into(),
                    detail_json: json!({}),
                },
                OperationStatus {
                    state: "MYSTERY".into(),
                    detail_json: json!({}),
                },
                OperationStatus {
                    state: "MYSTERY".into(),
                    detail_json: json!({}),
                },
            ],
            true,
        );
        let app = RankedApp {
            app_id: "app".into(),
            foreground_active_ms: 1,
            process_runtime_ms: 1,
            launch_count: 1,
            last_active_at: harness.clock.now_utc(),
        };
        let run = harness
            .database
            .create_run_with_frozen_apps(harness.clock.now_utc(), &[app])
            .unwrap();
        harness
            .database
            .mark_app_running(&run, "app", "persisted-operation", harness.clock.now_utc())
            .unwrap();
        harness.activity.latch_timeout();
        harness.engine.tick().await.unwrap();
        assert_eq!(harness.provider.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            harness.database.snapshot().unwrap().state,
            LifecycleState::BackupFailedSafe
        );
    }

    #[tokio::test]
    async fn no_apps_is_safe_failure() {
        let harness = harness(vec![], true);
        harness.clock.advance(Duration::from_secs(900));
        harness.engine.tick().await.unwrap();
        assert_eq!(harness.provider.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            harness.database.snapshot().unwrap().state,
            LifecycleState::BackupFailedSafe
        );
    }

    #[tokio::test]
    async fn activity_restarts_full_timeout_before_latch() {
        let harness = harness(vec![completed()], true);
        add_usage(&harness.database, &harness.clock, "app", 1_000);
        harness.clock.advance(Duration::from_secs(899));
        harness
            .activity
            .record_activity(crate::activity::ActivityEvent {
                event_type: "user_activity".into(),
                source: crate::activity::ActivitySource::Mouse,
                timestamp_ms: 1,
                synthetic: false,
                magnitude: None,
            })
            .await
            .unwrap();
        harness.clock.advance(Duration::from_secs(899));
        harness.engine.tick().await.unwrap();
        assert_eq!(harness.provider.calls.load(Ordering::SeqCst), 0);
    }
}
