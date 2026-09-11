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
use noland_discovery::{derive_owned_install_root, discover_all, SteamDiscovery};
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
    pub steam: Mutex<Option<SteamDiscovery>>,
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
            steam: Mutex::new(None),
            master_key: Mutex::new(None),
        })
    }

    pub fn recover(&self) -> Result<()> {
        let integrity = self.db.integrity_check()?;
        if integrity != "ok" {
            return Err(StateError::Database(integrity));
        }
        noland_storage::shred_all_ephemeral_sessions(&self.config.paths.run_root)?;
        let restore_recovery = noland_restore::recover_interrupted_restores(&self.config.paths)?;
        for failure in &restore_recovery.failures {
            tracing::error!(
                workspace = %failure.workspace.display(),
                error = %failure.error,
                "interrupted restore rollback failed; retaining recovery artifacts"
            );
        }
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

    fn attribution_engine(&self) -> noland_attribution::AttributionEngine<'_> {
        let roots = self.roots.lock().clone();
        let steam = self.steam.lock().clone();
        noland_attribution::AttributionEngine::new(&self.db, roots, self.config.paths.clone())
            .with_steam_discovery(steam)
    }

    pub fn discover(&self) -> Result<()> {
        let scan = discover_all(&self.config.home);
        for app in &scan.apps {
            self.db.upsert_app(app)?;
            if let Some(root) = derive_owned_install_root(&self.config.home, app) {
                self.db
                    .add_known_root(&app.app_id, "install", &root.to_string_lossy())?;
            }
            Metrics::inc(&self.metrics.apps_discovered_total);
        }
        *self.steam.lock() = scan.steam.clone();
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
                    // Keep the portable mapping available for explicitly selected saves/config,
                    // but do not register the generated Proton prefix as an app-wide scan root.
                    roots.proton_prefixes.insert(app.app_id, prefix.clone());
                }
            }
        }
        if let Some(steam) = &scan.steam {
            self.reconcile_completed_steam_installers(steam)?;
        }
        for prefix in scan.wine_prefixes {
            let path = std::fs::canonicalize(&prefix.path).unwrap_or(prefix.path);
            if let Some(app_id) = &prefix.associated_app {
                self.db
                    .add_known_root(app_id, "install", &path.to_string_lossy())?;
            }
            self.roots.lock().wine_prefixes.insert(prefix.id, path);
        }
        for bottle in scan.bottles {
            let path = std::fs::canonicalize(&bottle.path).unwrap_or(bottle.path);
            if let Some(app_id) = &bottle.associated_app {
                self.db
                    .add_known_root(app_id, "install", &path.to_string_lossy())?;
            }
            self.roots.lock().bottles_prefixes.insert(bottle.id, path);
        }
        Ok(())
    }

    fn reconcile_completed_steam_installers(&self, steam: &SteamDiscovery) -> Result<()> {
        for transaction in self.db.open_installers()? {
            if !matches!(
                transaction.transaction_type,
                InstallTransactionType::LauncherInstall | InstallTransactionType::LauncherUpdate
            ) {
                continue;
            }
            let Some(app) = steam
                .apps
                .iter()
                .find(|app| AppId::steam(app.app_id) == transaction.app_id)
            else {
                continue;
            };
            let staging_roots = transaction
                .candidate_roots
                .iter()
                .filter(|root| {
                    noland_discovery::classify_observed_path(root).disposition
                        == noland_discovery::PathDisposition::InstallStagingFile
                })
                .collect::<Vec<_>>();
            let staging_complete = !staging_roots.is_empty()
                && staging_roots
                    .into_iter()
                    .all(|root| staging_root_is_inactive(root));
            if !app.install_dir.exists() || !staging_complete {
                continue;
            }

            self.db.mark_dirty(&transaction.app_id, None, true)?;
            self.db.mark_dirty_root(
                &transaction.app_id,
                &app.install_dir.to_string_lossy(),
                None,
                true,
            )?;
            let indexed = crate::reconcile::reconcile_install_roots(self, &transaction.app_id)?;
            noland_attribution::finish_installer(&self.db, &transaction)?;
            tracing::info!(
                app_id = %transaction.app_id,
                transaction_id = %transaction.transaction_id,
                indexed,
                "completed launcher install and rebuilt final application index"
            );
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
        let mut engine = self.attribution_engine();
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
        let mut engine = self.attribution_engine();
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
            let mut process_recovery = tokio::time::interval(Duration::from_secs(2));
            let mut rediscovery = tokio::time::interval(Duration::from_secs(30));
            let mut checkpoint =
                tokio::time::interval(Duration::from_secs(constants::CHECKPOINT_INTERVAL_SECS));
            loop {
                tokio::select! {
                    _ = agent.hub.wait_for_events() => {
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

fn staging_root_is_inactive(root: &std::path::Path) -> bool {
    if !root.exists() {
        return true;
    }
    root.is_dir()
        && std::fs::read_dir(root)
            .ok()
            .is_some_and(|mut entries| entries.next().is_none())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use noland_restore::{RestorePlan, RestoreTarget, RestoreTransaction};
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

    #[test]
    fn recover_rolls_back_crashed_restore_before_marking_operation_interrupted() {
        let root = std::env::temp_dir().join(format!(
            "noland-crashed-restore-recover-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
        let operation_id = Uuid::new_v4();
        agent
            .db
            .upsert_operation(&OperationRecord {
                operation_id,
                kind: "restore".into(),
                app_id: Some(AppId::desktop("game")),
                state: RestoreState::Applying.as_str().into(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
                last_error: None,
                detail_json: serde_json::json!({"mode": "personal_state"}),
            })
            .unwrap();

        let payload = b"new-save";
        let hash = noland_cas::blake3_hex(payload);
        let mut manifest = BundleManifest::new(
            ManifestApp {
                app_id: AppId::desktop("game"),
                display_name: "Game".into(),
                aliases: Vec::new(),
                desktop_entry_id: None,
                steam_app_id: None,
                launcher: None,
                canonical_executable: None,
                icon_path: None,
            },
            ManifestSource {
                instance_id: Uuid::new_v4(),
                image_id: "test".into(),
                captured_at: Utc::now(),
            },
            BackupMode::PersonalState,
        );
        manifest.files.push(ManifestFile {
            logical_root: "$XDG_DATA_HOME".into(),
            relative_path: "game/save.dat".into(),
            source_path_hint: None,
            file_type: "file".into(),
            size: payload.len() as u64,
            file_hash: hash.clone(),
            chunks: vec![ChunkRef {
                hash: hash.clone(),
                size: payload.len() as u64,
            }],
            mode: None,
            mtime_ns: None,
            uid: None,
            gid: None,
            symlink_target: None,
            persistence_class: PersistenceClass::PersistentState,
            semantic_role: SemanticRole::UserState,
            association_confidence: 1.0,
            shared_app_ids: Vec::new(),
        });
        let restore_id = Uuid::new_v4();
        let staging = agent.config.paths.restore_dir(&restore_id.to_string());
        for child in ["packs", "materialized/.chunks", "pre_restore"] {
            std::fs::create_dir_all(staging.join(child)).unwrap();
        }
        std::fs::write(
            staging
                .join("materialized/.chunks")
                .join(hash.trim_start_matches("blake3:")),
            payload,
        )
        .unwrap();
        let plan = RestorePlan {
            restore_id,
            staging: staging.clone(),
            pack_cache: agent.config.paths.cache.join("restore-packs"),
            manifest,
            mode: RestoreMode::PersonalState,
        };
        let roots = LogicalRootMap::from_home(&agent.config.home);
        let destination = agent.config.home.join(".local/share/game/save.dat");
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::write(&destination, b"old-save").unwrap();
        let mut transaction = RestoreTransaction::new(&plan, &roots, None);
        transaction.publish_to(RestoreTarget::Complete).unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), payload);
        std::mem::forget(transaction);

        agent.recover().unwrap();

        assert_eq!(std::fs::read(&destination).unwrap(), b"old-save");
        assert!(!staging.exists());
        let operation = agent.db.get_operation(operation_id).unwrap().unwrap();
        assert_eq!(operation.state, "INTERRUPTED");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn discovery_is_available_to_attribution_engines() {
        let root = std::env::temp_dir().join(format!(
            "noland-agent-steam-discovery-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
        let steamapps = agent.config.home.join(".local/share/Steam/steamapps");
        let install_root = steamapps.join("common/Discovered Game");
        let executable = install_root.join("discovered-game");
        std::fs::create_dir_all(&install_root).unwrap();
        std::fs::write(&executable, b"game").unwrap();
        std::fs::write(
            steamapps.join("appmanifest_6060.acf"),
            r#"
            "AppState"
            {
                "appid" "6060"
                "name" "Discovered Game"
                "installdir" "Discovered Game"
            }
            "#,
        )
        .unwrap();

        agent.discover().unwrap();
        let engine = agent.attribution_engine();
        assert!(engine
            .steam
            .as_ref()
            .is_some_and(|steam| steam.apps.iter().any(|app| app.app_id == 6060)));
        drop(engine);

        agent.hub.inject_process(ProcessEvent {
            kind: ProcessEventKind::Exec,
            pid: 6060,
            ppid: 1,
            uid: 1000,
            gid: 1000,
            cgroup: None,
            executable: Some(executable),
            argv_hash: None,
            comm: Some("discovered-game".into()),
            at: Utc::now(),
        });
        assert_eq!(agent.process_events().unwrap(), 1);
        assert_eq!(
            agent.db.session_for_pid(6060).unwrap().unwrap().app_id,
            AppId::steam(6060)
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn discovery_persists_owned_roots_for_non_steam_apps() {
        let root = std::env::temp_dir().join(format!(
            "noland-agent-owned-roots-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
        let home = &agent.config.home;

        let native_executable = home.join("Applications/Native Game/bin/game");
        let portable_executable = home.join("Downloads/Portable.AppImage");
        let shared_executable = home.join(".local/share/applications/shared-helper");
        let applications = home.join(".local/share/applications");
        std::fs::create_dir_all(native_executable.parent().unwrap()).unwrap();
        std::fs::create_dir_all(portable_executable.parent().unwrap()).unwrap();
        std::fs::create_dir_all(&applications).unwrap();
        std::fs::write(&native_executable, b"native").unwrap();
        std::fs::write(&portable_executable, b"portable").unwrap();
        std::fs::write(&shared_executable, b"shared").unwrap();
        std::fs::write(
            applications.join("native-game.desktop"),
            format!(
                "[Desktop Entry]\nName=Native Game\nExec=\"{}\"\nType=Application\n",
                native_executable.display()
            ),
        )
        .unwrap();
        std::fs::write(
            applications.join("shared-helper.desktop"),
            format!(
                "[Desktop Entry]\nName=Shared Helper\nExec={}\nType=Application\n",
                shared_executable.display()
            ),
        )
        .unwrap();

        let wine_prefix = home.join(".wine");
        let bottle_prefix = home.join(".local/share/bottles/bottles/arcade");
        std::fs::create_dir_all(wine_prefix.join("drive_c")).unwrap();
        std::fs::create_dir_all(&bottle_prefix).unwrap();

        agent.discover().unwrap();

        let wine_id = AppId::launcher("wine", "default");
        let bottle_id = AppId::launcher("bottles", "arcade");
        assert_eq!(
            agent.db.get_app(&wine_id).unwrap().unwrap().launcher,
            Some(LauncherKind::Wine)
        );
        assert_eq!(
            agent.db.get_app(&bottle_id).unwrap().unwrap().launcher,
            Some(LauncherKind::Bottles)
        );
        assert_eq!(
            agent
                .db
                .get_app(&AppId::desktop("portable"))
                .unwrap()
                .unwrap()
                .launcher,
            Some(LauncherKind::AppImage)
        );

        let known_roots = agent.db.known_roots(None).unwrap();
        for (app_id, expected) in [
            (wine_id, wine_prefix),
            (bottle_id, bottle_prefix),
            (
                AppId::desktop("native-game"),
                home.join("Applications/Native Game"),
            ),
            (AppId::desktop("portable"), portable_executable),
        ]
        .map(|(app_id, path)| (app_id, std::fs::canonicalize(path).unwrap()))
        {
            assert!(
                known_roots.iter().any(|(root_app_id, kind, path)| {
                    root_app_id == &app_id && kind == "install" && PathBuf::from(path) == expected
                }),
                "missing durable root for {app_id}"
            );
        }
        assert!(!known_roots
            .iter()
            .any(|(app_id, _, _)| { app_id == &AppId::desktop("shared-helper") }));
        assert!(known_roots.iter().all(|(_, _, path)| {
            let path = PathBuf::from(path);
            path != *home && !path.starts_with("/usr") && !path.starts_with("/opt")
        }));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn discovery_finishes_steam_transaction_after_scanning_only_final_content() {
        let root = std::env::temp_dir().join(format!(
            "noland-agent-install-complete-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
        let steamapps = agent.config.home.join(".local/share/Steam/steamapps");
        let final_root = steamapps.join("common/Completed Game");
        let final_file = final_root.join("content/deep/asset.pak");
        let staging_root = steamapps.join("downloading/7070");
        std::fs::create_dir_all(final_file.parent().unwrap()).unwrap();
        std::fs::write(&final_file, b"complete asset").unwrap();
        std::fs::write(
            steamapps.join("appmanifest_7070.acf"),
            r#"
            "AppState"
            {
                "appid" "7070"
                "name" "Completed Game"
                "installdir" "Completed Game"
            }
            "#,
        )
        .unwrap();
        noland_attribution::start_installer(
            &agent.db,
            AppId::steam(7070),
            None,
            vec![staging_root.clone()],
            InstallTransactionType::LauncherInstall,
        )
        .unwrap();

        agent.discover().unwrap();

        assert!(agent.db.open_installers().unwrap().is_empty());
        assert!(agent
            .db
            .known_roots(Some(&AppId::steam(7070)))
            .unwrap()
            .iter()
            .all(|(_, _, path)| PathBuf::from(path) != staging_root));
        let record = agent
            .db
            .get_path_by_canonical(&final_file.to_string_lossy())
            .unwrap()
            .expect("completed install should be indexed");
        let association = agent
            .db
            .associations_for_path(record.path_id)
            .unwrap()
            .into_iter()
            .find(|association| association.app_id == AppId::steam(7070))
            .expect("completed install should belong to its Steam app");
        assert_eq!(
            association.persistence_class,
            PersistenceClass::ReconstructableApp
        );
        assert_eq!(association.semantic_role, SemanticRole::AppContent);
        assert!(
            !agent
                .db
                .list_dirty_apps()
                .unwrap()
                .into_iter()
                .find(|dirty| dirty.app_id == AppId::steam(7070))
                .unwrap()
                .requires_reconciliation
        );

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
