//! Noland remote state agent: always-on tracking, backup, restore, seal.

pub mod backup;
pub mod checkpoint;
pub mod observer;
pub mod operation_manager;
pub mod reconcile;
pub mod restore;
pub mod rpc_handler;
pub mod seal;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use noland_baseline::current_image_id;
use noland_discovery::discover_all;
use noland_observer::ObserverHub;
use noland_state_core::metrics::Metrics;
use noland_state_core::*;
use noland_state_db::StateDb;
use parking_lot::Mutex;
use uuid::Uuid;

pub struct AgentConfig {
    pub instance_id: Uuid,
    pub image_id: String,
    pub paths: AgentPaths,
    pub home: PathBuf,
    pub user: String,
}

impl AgentConfig {
    pub fn from_env() -> Self {
        let state_root = std::env::var("NOLAND_STATE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(constants::STATE_ROOT));
        let run_root = std::env::var("NOLAND_RUN_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(constants::RUN_ROOT));
        let home = std::env::var("NOLAND_HOME")
            .or_else(|_| std::env::var("HOME"))
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/home/gamer"));
        let instance_id = std::env::var("NOLAND_INSTANCE_ID")
            .ok()
            .and_then(|s| Uuid::parse_str(&s).ok())
            .unwrap_or_else(Uuid::new_v4);
        Self {
            instance_id,
            image_id: current_image_id(),
            paths: AgentPaths::from_roots(state_root, run_root),
            home,
            user: std::env::var("USER").unwrap_or_else(|_| "gamer".into()),
        }
    }

    pub fn isolated(root: PathBuf) -> Self {
        Self {
            instance_id: Uuid::new_v4(),
            image_id: "test-image".into(),
            paths: AgentPaths::from_roots(root.join("state"), root.join("run")),
            home: root.join("home"),
            user: "gamer".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct LiveProcessRecovery {
    pub processes_seen: usize,
    pub sessions_pruned: usize,
    pub sessions_recovered: usize,
    pub open_files_recovered: usize,
}

pub struct StateAgent {
    pub config: AgentConfig,
    pub db: StateDb,
    pub hub: Arc<ObserverHub>,
    pub metrics: Arc<Metrics>,
    pub observer: observer::ObserverSupervisor,
    pub operations: operation_manager::OperationManager,
    pub roots: Mutex<LogicalRootMap>,
    pub master_key: Mutex<Option<noland_crypto::MasterKey>>,
}

impl StateAgent {
    pub fn boot(config: AgentConfig) -> Result<Self> {
        config.paths.ensure_dirs()?;
        let db = StateDb::open(&config.paths.db_path)?;
        let metrics = Metrics::shared();
        let hub = Arc::new(ObserverHub::new(metrics.clone()));
        let roots = LogicalRootMap::from_home(&config.home);
        Ok(Self {
            config,
            db,
            hub,
            metrics,
            observer: observer::ObserverSupervisor::new(),
            operations: operation_manager::OperationManager::default(),
            roots: Mutex::new(roots),
            master_key: Mutex::new(None),
        })
    }

    pub fn recover(&self) -> Result<()> {
        let integrity = self.db.integrity_check()?;
        if integrity != "ok" {
            return Err(StateError::Database(integrity));
        }
        noland_storage::shred_all_ephemeral_sessions(&self.config.paths.run_root)?;
        for mut op in self.db.unfinished_operations()? {
            let journal = self.db.sync_journal_summary(op.operation_id)?;
            let has_retry = matches!(op.kind.as_str(), "backup" | "restore");
            let resume_mode = if journal.completed_items > 0 && op.kind == "backup" {
                "UPLOAD_JOURNAL"
            } else {
                "NEEDS_CLIENT_RESUBMIT"
            };
            tracing::warn!(
                operation_id = %op.operation_id,
                kind = %op.kind,
                state = %op.state,
                resume_mode,
                completed_journal_items = journal.completed_items,
                retry_available = has_retry,
                "unfinished operation after restart; preserving it for retry"
            );
            op.state = "INTERRUPTED".into();
            op.updated_at = chrono::Utc::now();
            op.last_error = Some(
                "operation interrupted by state-agent restart; retry with a new session or resubmit the original request".into(),
            );
            if !op.detail_json.is_object() {
                op.detail_json = serde_json::json!({});
            }
            if let Some(fields) = op.detail_json.as_object_mut() {
                fields.insert(
                    "resume".into(),
                    serde_json::json!({
                        "reason": "agent_restart",
                        "mode": resume_mode,
                        "completed_journal_items": journal.completed_items,
                        "retry_available": has_retry,
                    }),
                );
            }
            self.db.upsert_operation(&op)?;
        }
        for event in noland_observer::bootstrap_from_procfs() {
            self.hub.inject_process(event);
        }
        for dirty in self.db.list_dirty_apps()? {
            self.db.mark_dirty(&dirty.app_id, None, true)?;
        }
        Ok(())
    }

    pub fn discover(&self) -> Result<()> {
        let scan = discover_all(&self.config.home);
        for app in &scan.apps {
            self.db.upsert_app(app)?;
            Metrics::inc(&self.metrics.apps_discovered_total);
        }
        if let Some(steam) = &scan.steam {
            let mut roots = self.roots.lock();
            roots.steam_root = Some(steam.root.clone());
            for (id, lib) in &steam.libraries {
                roots.steam_libraries.insert(id.clone(), lib.clone());
            }
            for app in &steam.apps {
                self.db.add_known_root(
                    &AppId::steam(app.app_id),
                    "install",
                    &app.install_dir.to_string_lossy(),
                )?;
                if let Some(prefix) = &app.prefix {
                    self.db.add_known_root(
                        &AppId::steam(app.app_id),
                        "proton",
                        &prefix.to_string_lossy(),
                    )?;
                    roots.proton_prefixes.insert(app.app_id, prefix.clone());
                }
            }
        }
        for prefix in scan.wine_prefixes {
            self.roots
                .lock()
                .wine_prefixes
                .insert(prefix.id.clone(), prefix.path);
        }
        for bottle in scan.bottles {
            self.roots
                .lock()
                .bottles_prefixes
                .insert(bottle.id.clone(), bottle.path);
        }
        Ok(())
    }

    /// Rebuilds sessions for live processes missed by the kernel event stream and
    /// recovers dependency evidence from their currently open regular files.
    pub fn reconcile_live_processes(&self) -> Result<LiveProcessRecovery> {
        let events = noland_observer::bootstrap_from_procfs();
        let mut recovery = LiveProcessRecovery {
            processes_seen: events.len(),
            sessions_pruned: self.prune_stale_sessions()?,
            ..LiveProcessRecovery::default()
        };
        let mut engine = noland_attribution::AttributionEngine::new(
            &self.db,
            self.roots.lock().clone(),
            self.config.paths.clone(),
        );
        for event in events {
            let had_session = self.db.session_for_pid(event.pid)?.is_some();
            let session = engine.ingest_process(&event)?;
            if session.is_none() {
                continue;
            }
            if had_session {
                continue;
            }
            recovery.sessions_recovered = recovery.sessions_recovered.saturating_add(1);
            for path in noland_observer::open_files_from_procfs(event.pid) {
                let fact = FilesystemEvent {
                    kind: FsEventKind::Open,
                    pid: event.pid,
                    path,
                    dest_path: None,
                    at: chrono::Utc::now(),
                    sampled: false,
                };
                if engine.ingest_fs(&fact)?.is_some() {
                    recovery.open_files_recovered = recovery.open_files_recovered.saturating_add(1);
                }
            }
        }
        Ok(recovery)
    }

    fn prune_stale_sessions(&self) -> Result<usize> {
        #[cfg(not(target_os = "linux"))]
        {
            return Ok(0);
        }

        #[cfg(target_os = "linux")]
        {
            let mut pruned = 0usize;
            for session in self.db.open_sessions()? {
                let pids = self.db.session_pids(session.session_id)?;
                let mut has_live_pid = false;
                for pid in pids {
                    if PathBuf::from(format!("/proc/{pid}")).exists() {
                        has_live_pid = true;
                    } else {
                        self.db.detach_pid(pid)?;
                    }
                }
                if !has_live_pid {
                    self.db.end_session(session.session_id)?;
                    pruned = pruned.saturating_add(1);
                }
            }
            Ok(pruned)
        }
    }

    pub fn process_events(&self) -> Result<usize> {
        let mut engine = noland_attribution::AttributionEngine::new(
            &self.db,
            self.roots.lock().clone(),
            self.config.paths.clone(),
        );
        let n = noland_attribution::process_hub_events(&mut engine, &self.hub)?;
        if self.hub.queue.take_loss_flag() {
            let dropped = self.hub.queue.dropped();
            let app_ids = self.db.open_session_app_ids()?;
            for app_id in &app_ids {
                self.db.mark_dirty(app_id, None, true)?;
                let known_roots = self.db.known_roots(Some(app_id))?;
                if known_roots.is_empty() {
                    self.db.mark_dirty_root(
                        app_id,
                        &self.config.home.to_string_lossy(),
                        None,
                        true,
                    )?;
                } else {
                    for (_, _, root) in known_roots {
                        self.db.mark_dirty_root(app_id, &root, None, true)?;
                    }
                }
                for state in self.db.list_file_states(app_id, None)? {
                    let _ = self.db.set_file_state_trust(
                        app_id,
                        &state.logical_root,
                        &state.relative_path,
                        FileStateTrust::VerifyRequired,
                    )?;
                }
            }
            self.observer.signal_loss(
                "agent_queue",
                format!(
                    "{dropped} total events dropped; {} active apps marked for reconciliation",
                    app_ids.len()
                ),
            );
        }
        Ok(n)
    }

    pub fn start_observer(&self) {
        self.observer.start(Arc::clone(&self.hub));
    }

    pub fn spawn_background(self: &Arc<Self>) {
        let agent = Arc::clone(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(250));
            let mut process_recovery = tokio::time::interval(Duration::from_secs(2));
            let mut rediscovery = tokio::time::interval(Duration::from_secs(30));
            let mut checkpoint =
                tokio::time::interval(Duration::from_secs(constants::CHECKPOINT_INTERVAL_SECS));
            loop {
                tokio::select! {
                    _ = tick.tick() => {
                        if let Err(err) = agent.process_events() {
                            tracing::warn!(error = %err, "event processing failed");
                        }
                    }
                    _ = process_recovery.tick() => {
                        match agent.reconcile_live_processes() {
                            Ok(recovery)
                                if recovery.sessions_pruned > 0 || recovery.sessions_recovered > 0 =>
                            {
                                tracing::info!(
                                    sessions_pruned = recovery.sessions_pruned,
                                    sessions_recovered = recovery.sessions_recovered,
                                    open_files_recovered = recovery.open_files_recovered,
                                    "reconciled application sessions from procfs"
                                );
                            }
                            Ok(_) => {}
                            Err(err) => tracing::warn!(error = %err, "live process reconciliation failed"),
                        }
                    }
                    _ = rediscovery.tick() => {
                        if let Err(err) = agent.discover() {
                            tracing::warn!(error = %err, "periodic application discovery failed");
                        }
                    }
                    _ = checkpoint.tick() => {
                        if let Err(err) = crate::checkpoint::maybe_checkpoint(&agent) {
                            tracing::warn!(error = %err, "checkpoint failed");
                        }
                    }
                }
            }
        });
    }

    pub async fn serve_rpc(self: &Arc<Self>) -> Result<()> {
        let listener = noland_rpc::bind_socket(&self.config.paths.rpc_socket).await?;
        tracing::info!(socket = %self.config.paths.rpc_socket.display(), "rpc listening");
        loop {
            let (stream, _) = listener.accept().await?;
            let agent = Arc::clone(self);
            tokio::spawn(async move {
                let handler = crate::rpc_handler::AgentRpc(agent);
                if let Err(err) = noland_rpc::serve_connection(stream, handler).await {
                    tracing::warn!(error = %err, "rpc connection ended");
                }
            });
        }
    }

    pub fn install_master_key(&self, key: noland_crypto::MasterKey) {
        *self.master_key.lock() = Some(key);
    }

    pub fn take_master_key(&self) -> Option<noland_crypto::MasterKey> {
        self.master_key.lock().take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use noland_state_core::{ContentObjectKind, SyncDirection, SyncJournalEntry, SyncJournalState};

    #[test]
    fn recover_preserves_unfinished_operations_as_interrupted() {
        let root = std::env::temp_dir().join(format!(
            "noland-recover-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
        let operation_id = Uuid::new_v4();
        agent
            .db
            .upsert_operation(&OperationRecord {
                operation_id,
                kind: "backup".into(),
                app_id: Some(AppId::desktop("game")),
                state: BackupState::Uploading.as_str().into(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
                last_error: None,
                detail_json: serde_json::json!({
                    "app_id": "desktop:game",
                    "mode": "personal_state",
                    "performance_mode": "balanced"
                }),
            })
            .unwrap();
        let mut entry = SyncJournalEntry::pending(
            operation_id,
            "packs/ab/pack.pack",
            ContentObjectKind::Pack,
            SyncDirection::Upload,
        );
        entry.state = SyncJournalState::Completed;
        agent.db.upsert_sync_journal_entry(&entry).unwrap();
        let mut progress = OperationProgress::new("uploading", 7);
        progress.message = Some("uploading packs".into());
        agent
            .db
            .set_operation_progress(operation_id, Some(&progress))
            .unwrap();

        agent.recover().unwrap();

        let recovered = agent.db.get_operation(operation_id).unwrap().unwrap();
        assert_eq!(recovered.state, "INTERRUPTED");
        assert_eq!(
            recovered.detail_json["resume"]["mode"],
            serde_json::json!("UPLOAD_JOURNAL")
        );
        assert_eq!(
            recovered.detail_json["resume"]["retry_available"],
            serde_json::json!(true)
        );
        let recovered_progress = agent
            .db
            .get_operation_progress(operation_id)
            .unwrap()
            .unwrap();
        assert_eq!(recovered_progress.phase, "uploading");
        assert_eq!(recovered_progress.completed_units, 7);

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn live_reconciliation_prunes_only_sessions_without_live_pids() {
        let root = std::env::temp_dir().join(format!(
            "noland-session-prune-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
        let live_app = AppIdentity::new(AppId::desktop("live-app"), "Live App");
        let stale_app = AppIdentity::new(AppId::desktop("stale-app"), "Stale App");
        agent.db.upsert_app(&live_app).unwrap();
        agent.db.upsert_app(&stale_app).unwrap();

        let live_session = AppSession::new(
            live_app.app_id,
            std::process::id() as i32,
            SessionSource::ExecutableDiscovery,
        );
        let stale_session = AppSession::new(
            stale_app.app_id,
            i32::MAX,
            SessionSource::ExecutableDiscovery,
        );
        agent.db.insert_session(&live_session).unwrap();
        agent.db.insert_session(&stale_session).unwrap();

        assert_eq!(agent.prune_stale_sessions().unwrap(), 1);
        assert!(agent
            .db
            .open_session_for_app(&live_session.app_id)
            .unwrap()
            .is_some());
        assert!(agent
            .db
            .open_session_for_app(&stale_session.app_id)
            .unwrap()
            .is_none());

        let _ = std::fs::remove_dir_all(root);
    }
}
