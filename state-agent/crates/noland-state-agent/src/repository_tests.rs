use super::*;
use async_trait::async_trait;
use noland_storage::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

pub(crate) struct CredentialsProvider {
    inner: LocalStorage,
    config: PathBuf,
    pub(crate) delete_calls: Arc<AtomicUsize>,
    pub(crate) fail_delete: Arc<AtomicBool>,
    pub(crate) retain_claim: Arc<AtomicBool>,
    fail_stat: Arc<AtomicBool>,
    pub(crate) release_started: Option<Arc<tokio::sync::Notify>>,
    pub(crate) release_continue: Option<Arc<tokio::sync::Notify>>,
}

impl CredentialsProvider {
    pub(crate) fn new(root: PathBuf, config: PathBuf) -> Self {
        Self {
            inner: LocalStorage::new(root),
            config,
            delete_calls: Arc::default(),
            fail_delete: Arc::default(),
            retain_claim: Arc::default(),
            fail_stat: Arc::default(),
            release_started: None,
            release_continue: None,
        }
    }
}

#[async_trait]
impl SharedStorageProvider for CredentialsProvider {
    async fn health_check(&self) -> Result<Health> {
        self.inner.health_check().await
    }
    async fn ensure_root(&self) -> Result<()> {
        self.inner.ensure_root().await
    }
    async fn stat(&self, key: &RemoteKey) -> Result<Option<RemoteMeta>> {
        assert!(
            self.config.is_file(),
            "credentials must outlive claim absence verification"
        );
        if self.fail_stat.load(Ordering::SeqCst) {
            return Err(StateError::Storage(
                "private-stat-request-must-not-be-archived".into(),
            ));
        }
        self.inner.stat(key).await
    }
    async fn upload_immutable(&self, local: &Path, key: &RemoteKey) -> Result<RemoteMeta> {
        self.inner.upload_immutable(local, key).await
    }
    async fn download(&self, key: &RemoteKey, dest: &Path) -> Result<()> {
        self.inner.download(key, dest).await
    }
    async fn read_small(&self, key: &RemoteKey, limit: u64) -> Result<Vec<u8>> {
        self.inner.read_small(key, limit).await
    }
    async fn list_prefix(&self, key: &RemoteKey) -> Result<Vec<RemoteEntry>> {
        self.inner.list_prefix(key).await
    }
    async fn put_small_versioned(
        &self,
        bytes: bytes::Bytes,
        key: &RemoteKey,
    ) -> Result<RemoteMeta> {
        self.inner.put_small_versioned(bytes, key).await
    }
    async fn delete_object(&self, key: &RemoteKey) -> Result<()> {
        assert!(
            self.config.is_file(),
            "credentials must outlive the final claim delete"
        );
        if key.as_str().starts_with("maintenance/activity/") {
            self.delete_calls.fetch_add(1, Ordering::SeqCst);
            if let Some(started) = &self.release_started {
                started.notify_one();
                if let Some(resume) = &self.release_continue {
                    resume.notified().await;
                } else {
                    std::future::pending::<()>().await;
                }
            }
            if self.fail_delete.load(Ordering::SeqCst) {
                return Err(StateError::Storage(
                    "private-token-must-not-be-archived".into(),
                ));
            }
            if self.retain_claim.load(Ordering::SeqCst) {
                return Ok(());
            }
        }
        self.inner.delete_object(key).await
    }
}

fn operation(agent: &StateAgent, id: Uuid, kind: &str, state: &str) {
    agent
        .db
        .upsert_operation(&OperationRecord {
            operation_id: id,
            kind: kind.into(),
            app_id: None,
            state: state.into(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            last_error: (state == "FAILED").then(|| "original transfer failure".into()),
            detail_json: serde_json::json!({}),
        })
        .unwrap();
}

fn admission(agent: &StateAgent, id: Uuid) -> serde_json::Value {
    agent.db.get_operation(id).unwrap().unwrap().detail_json["repository_admission"].clone()
}

#[tokio::test]
async fn different_instances_read_together_without_clearing_each_others_claims() {
    let root = crate::index_test_support::root("instance-admission");
    let first = StateAgent::boot(crate::AgentConfig::isolated(root.join("first"))).unwrap();
    let second = StateAgent::boot(crate::AgentConfig::isolated(root.join("second"))).unwrap();
    let writer = StateAgent::boot(crate::AgentConfig::isolated(root.join("writer"))).unwrap();
    let storage = LocalStorage::new(root.join("cloud"));
    let master = MasterKey::generate();
    let first_id = Uuid::new_v4();
    operation(&first, first_id, "restore", "APPLYING");
    let first_guard = acquire_restore(&first, &storage, &master, first_id)
        .await
        .unwrap();
    assert!(acquire_restore(&first, &storage, &master, Uuid::new_v4())
        .await
        .is_err());
    let second_guard = acquire_restore(&second, &storage, &master, Uuid::new_v4())
        .await
        .unwrap();
    let activities = repository_activities(&storage, &master).await.unwrap();
    assert_eq!(activities.len(), 2);
    assert!(activities
        .iter()
        .all(|claim| claim.access == RepositoryAccess::RestoreRead));
    assert_ne!(activities[0].instance_id, activities[1].instance_id);
    assert!(acquire(&writer, &storage, &master, Uuid::new_v4())
        .await
        .is_err());
    assert_eq!(
        repository_activities(&storage, &master).await.unwrap(),
        activities
    );
    first_guard.release().await.unwrap();
    assert_eq!(admission(&first, first_id)["status"], "RELEASED");
    assert_eq!(admission(&first, first_id)["access"], "restore_read");
    assert_eq!(
        repository_activities(&storage, &master)
            .await
            .unwrap()
            .len(),
        1
    );
    second_guard.release().await.unwrap();
    let exclusive = acquire(&writer, &storage, &master, Uuid::new_v4())
        .await
        .unwrap();
    assert_eq!(
        repository_activities(&storage, &master).await.unwrap()[0].access,
        RepositoryAccess::Exclusive
    );
    exclusive.release().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn durable_identity_recovers_only_own_scoped_and_legacy_claims_after_restart() {
    let root = crate::index_test_support::root("instance-restart");
    let config_root = root.join("agent");
    let first = StateAgent::boot(crate::AgentConfig::isolated(config_root.clone())).unwrap();
    let master = MasterKey::generate();
    let storage = LocalStorage::new(root.join("cloud"));
    let durable = Uuid::new_v4();
    let spelling = durable.to_string().to_uppercase();
    std::fs::write(
        first.config.paths.state_root.join("repository-owner-id"),
        &spelling,
    )
    .unwrap();
    let expected = blake3::keyed_hash(&master.0, spelling.as_bytes())
        .to_hex()
        .to_string();
    assert_eq!(owner(&first, &master).unwrap(), expected);
    let daemon_id = first.config.instance_id;
    let abandoned_id = Uuid::new_v4();
    operation(&first, abandoned_id, "restore", "COMPLETED");
    drop(
        acquire_restore(&first, &storage, &master, abandoned_id)
            .await
            .unwrap(),
    );
    assert_eq!(
        admission(&first, abandoned_id)["status"],
        "CLEANUP_REQUIRED"
    );
    drop(first);
    let second = StateAgent::boot(crate::AgentConfig::isolated(config_root)).unwrap();
    assert_ne!(second.config.instance_id, daemon_id);
    let guard = acquire_restore(&second, &storage, &master, Uuid::new_v4())
        .await
        .unwrap();
    assert_eq!(
        repository_activities(&storage, &master).await.unwrap()[0].instance_id,
        Some(durable)
    );
    assert_eq!(admission(&second, abandoned_id)["status"], "RECOVERED");
    assert_eq!(
        second
            .db
            .get_operation(abandoned_id)
            .unwrap()
            .unwrap()
            .state,
        "COMPLETED"
    );
    guard.release().await.unwrap();
    drop(
        RepositoryGuard::acquire(&storage, &master, Uuid::new_v4(), &expected)
            .await
            .unwrap(),
    );
    acquire_restore(&second, &storage, &master, Uuid::new_v4())
        .await
        .unwrap()
        .release()
        .await
        .unwrap();

    // Even a matching owner hash is not authority to clear another instance.
    let foreign = RepositoryActivity {
        claim_id: Uuid::new_v4(),
        operation_id: Uuid::new_v4(),
        owner: expected.clone(),
        access: RepositoryAccess::RestoreRead,
        instance_id: Some(Uuid::new_v4()),
    };
    put_claim(&storage, &master, &foreign).await;
    assert!(acquire_restore(&second, &storage, &master, Uuid::new_v4())
        .await
        .is_err());
    assert_eq!(
        repository_activities(&storage, &master).await.unwrap(),
        vec![foreign.clone()]
    );
    clear_repository_activity(&storage, &master, &foreign)
        .await
        .unwrap();
    let unknown = RepositoryGuard::acquire(&storage, &master, Uuid::new_v4(), "legacy-other-pc")
        .await
        .unwrap();
    assert!(acquire_restore(&second, &storage, &master, Uuid::new_v4())
        .await
        .is_err());
    unknown.release().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

async fn put_claim(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    claim: &RepositoryActivity,
) {
    provider
        .put_small_versioned(
            seal_metadata(
                &noland_crypto::derive_keys(master).catalog,
                b"repository-activity-v1",
                claim,
            )
            .unwrap()
            .envelope
            .into(),
            &RemoteKey::new(format!("maintenance/activity/{}.enc", claim.claim_id)),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn release_failure_is_durable_separate_and_never_retried_or_replaces_transfer_error() {
    let root = crate::index_test_support::root("instance-release-errors");
    let agent = StateAgent::boot(crate::AgentConfig::isolated(root.join("agent"))).unwrap();
    let master = MasterKey::generate();
    let session = crate::index_test_support::session(&root.join("cloud"));
    let (config, credentials) =
        write_guarded_ephemeral_session(&agent.config.paths.run_root, &session).unwrap();
    let provider = CredentialsProvider::new(root.join("cloud"), config.clone());
    for (kind, state) in [
        ("restore", "COMPLETED"),
        ("backup", "COMPLETED"),
        ("seal", "SEALED"),
        ("delete", "COMPLETED"),
        ("restore", "FAILED"),
    ] {
        let id = Uuid::new_v4();
        operation(&agent, id, kind, state);
        let guard = if kind == "restore" {
            acquire_restore(&agent, &provider, &master, id)
                .await
                .unwrap()
        } else {
            acquire(&agent, &provider, &master, id).await.unwrap()
        };
        provider.fail_delete.store(true, Ordering::SeqCst);
        let calls = provider.delete_calls.load(Ordering::SeqCst);
        let original = if state == "FAILED" {
            Err(StateError::Integrity("original transfer failure".into()))
        } else {
            Ok(())
        };
        let result = guard.finish(original).await;
        assert_eq!(provider.delete_calls.load(Ordering::SeqCst), calls + 1);
        assert_eq!(result.is_ok(), state != "FAILED");
        if let Err(error) = result {
            assert!(matches!(error, StateError::Integrity(_)));
            assert!(error.to_string().contains("original transfer failure"));
        }
        let saved = agent.db.get_operation(id).unwrap().unwrap();
        assert_eq!(saved.state, state);
        assert_eq!(admission(&agent, id)["status"], "CLEANUP_REQUIRED");
        assert_eq!(admission(&agent, id)["reason"], "release_failed");
        assert_eq!(admission(&agent, id)["cleanup_required"], true);
        assert!(!saved.detail_json.to_string().contains("private-token"));
        assert!(config.is_file());
        provider.fail_delete.store(false, Ordering::SeqCst);
        // A new execution proves stoppage under the unchanged local lock.
        acquire(&agent, &provider, &master, Uuid::new_v4())
            .await
            .unwrap()
            .release()
            .await
            .unwrap();
        assert_eq!(admission(&agent, id)["status"], "RECOVERED");
    }
    drop(credentials);
    assert!(!config.parent().unwrap().exists());
    assert!(repository_activities(&provider.inner, &master)
        .await
        .unwrap()
        .is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn cleanup_stat_and_delete_errors_never_expose_provider_requests_or_config_paths() {
    let root = crate::index_test_support::root("instance-safe-cleanup-errors");
    let agent = StateAgent::boot(crate::AgentConfig::isolated(root.join("agent"))).unwrap();
    let master = MasterKey::generate();
    let session = crate::index_test_support::session(&root.join("cloud"));
    let (config, credentials) =
        write_guarded_ephemeral_session(&agent.config.paths.run_root, &session).unwrap();
    let provider = CredentialsProvider::new(root.join("cloud"), config.clone());
    for stat_failure in [true, false] {
        let id = Uuid::new_v4();
        operation(&agent, id, "restore", "COMPLETED");
        let guard = acquire_restore(&agent, &provider, &master, id)
            .await
            .unwrap();
        provider.fail_stat.store(stat_failure, Ordering::SeqCst);
        provider.fail_delete.store(!stat_failure, Ordering::SeqCst);
        let error = guard.release().await.unwrap_err().to_string();
        assert_eq!(error, cleanup_error().to_string());
        assert!(!error.contains("private-"));
        assert!(error.len() < 512);
        let saved = agent.db.get_operation(id).unwrap().unwrap();
        let progress = agent.db.get_operation_progress(id).unwrap().unwrap();
        for detail in [saved.detail_json, progress.detail_json] {
            let archived = detail.to_string();
            assert!(!archived.contains("private-"));
            assert!(!archived.contains(config.to_str().unwrap()));
            assert_eq!(detail["repository_admission"]["code"], CLEANUP_CODE);
            assert_eq!(detail["repository_admission"]["detail"], CLEANUP_DETAIL);
        }
        // The same restriction applies when recovering this execution's claim:
        // its stat/control failures are cleanup errors, not a transfer error.
        let recovery_error = match acquire_restore(&agent, &provider, &master, Uuid::new_v4()).await
        {
            Err(error) => error,
            Ok(_) => panic!("fixture cleanup must still fail"),
        };
        assert_eq!(recovery_error.to_string(), cleanup_error().to_string());
        assert_eq!(admission(&agent, id)["reason"], "owner_recovery_failed");
        provider.fail_stat.store(false, Ordering::SeqCst);
        provider.fail_delete.store(false, Ordering::SeqCst);
        acquire_restore(&agent, &provider, &master, Uuid::new_v4())
            .await
            .unwrap()
            .release()
            .await
            .unwrap();
    }
    drop(credentials);
    assert!(!config.exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn cancellation_records_cleanup_before_shredding_and_recovers_only_after_workers_stop() {
    let root = crate::index_test_support::root("instance-cancel");
    let agent =
        Arc::new(StateAgent::boot(crate::AgentConfig::isolated(root.join("agent"))).unwrap());
    let id = Uuid::new_v4();
    operation(&agent, id, "restore", "APPLYING");
    let master = MasterKey::generate();
    let session = crate::index_test_support::session(&root.join("cloud"));
    let task_agent = agent.clone();
    let task_master = master.clone();
    let cloud = root.join("cloud");
    let task_cloud = cloud.clone();
    let (started, ready) = tokio::sync::oneshot::channel();
    let (cancelled, stopped) = tokio::sync::oneshot::channel();
    assert!(agent.operations.spawn_cancellable_on_lane(
        id,
        crate::operation_manager::OperationLane::Transfer,
        async move {
            let (config, _credentials) =
                write_guarded_ephemeral_session(&task_agent.config.paths.run_root, &session)
                    .unwrap();
            let provider = CredentialsProvider::new(task_cloud, config.clone());
            let _guard = acquire_restore(&task_agent, &provider, &task_master, id)
                .await
                .unwrap();
            started
                .send((config, provider.delete_calls.clone()))
                .unwrap();
            std::future::pending::<()>().await;
        },
        move || {
            let _ = cancelled.send(());
        },
        |_| panic!("cancellation is not a worker panic"),
    ));
    let (config, deletes) = ready.await.unwrap();
    assert!(config.is_file());
    agent.operations.cancel(id);
    stopped.await.unwrap();
    assert!(!config.parent().unwrap().exists());
    assert_eq!(
        deletes.load(Ordering::SeqCst),
        0,
        "drop must not detach an unowned cloud delete"
    );
    assert_eq!(
        admission(&agent, id)["reason"],
        "execution_stopped_before_release"
    );
    let storage = LocalStorage::new(cloud.clone());
    assert_eq!(
        repository_activities(&storage, &master)
            .await
            .unwrap()
            .len(),
        1
    );
    let session = crate::index_test_support::session(&cloud);
    let (config, credentials) =
        write_guarded_ephemeral_session(&agent.config.paths.run_root, &session).unwrap();
    let provider = CredentialsProvider::new(cloud, config.clone());
    acquire_restore(&agent, &provider, &master, Uuid::new_v4())
        .await
        .unwrap()
        .release()
        .await
        .unwrap();
    assert_eq!(admission(&agent, id)["status"], "RECOVERED");
    drop(credentials);
    assert!(!config.exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn interrupted_release_keeps_durable_completion_and_local_lock_until_finalizer_stops() {
    let root = crate::index_test_support::root("instance-cancel-release");
    let agent = StateAgent::boot(crate::AgentConfig::isolated(root.join("agent"))).unwrap();
    let master = MasterKey::generate();
    let id = Uuid::new_v4();
    operation(&agent, id, "restore", "COMPLETED");
    let session = crate::index_test_support::session(&root.join("cloud"));
    let (config, credentials) =
        write_guarded_ephemeral_session(&agent.config.paths.run_root, &session).unwrap();
    let mut provider = CredentialsProvider::new(root.join("cloud"), config.clone());
    let started = Arc::new(tokio::sync::Notify::new());
    provider.release_started = Some(started.clone());
    let guard = acquire_restore(&agent, &provider, &master, id)
        .await
        .unwrap();
    let mut release = Box::pin(guard.release());
    tokio::select! {
        _ = &mut release => panic!("release must be blocked"),
        _ = started.notified() => {}
    }
    assert!(acquire(&agent, &provider, &master, Uuid::new_v4())
        .await
        .is_err());
    assert!(config.is_file());
    drop(release);
    assert_eq!(admission(&agent, id)["status"], "CLEANUP_REQUIRED");
    assert_eq!(
        agent.db.get_operation(id).unwrap().unwrap().state,
        "COMPLETED"
    );
    assert_eq!(provider.delete_calls.load(Ordering::SeqCst), 1);
    drop(credentials);
    assert!(!config.exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn group_child_cleanup_is_visible_on_parent_and_preserves_completed_child() {
    let root = crate::index_test_support::root("instance-group-cleanup");
    let agent = StateAgent::boot(crate::AgentConfig::isolated(root.join("agent"))).unwrap();
    let master = MasterKey::generate();
    let session = crate::index_test_support::session(&root.join("cloud"));
    let (config, credentials) =
        write_guarded_ephemeral_session(&agent.config.paths.run_root, &session).unwrap();
    let provider = CredentialsProvider::new(root.join("cloud"), config.clone());
    for kind in ["backup", "seal"] {
        let parent = Uuid::new_v4();
        let child = Uuid::new_v4();
        operation(&agent, parent, kind, "RUNNING");
        operation(&agent, child, "backup", "COMPLETED");
        let mut op = agent.db.get_operation(child).unwrap().unwrap();
        op.detail_json["parent_operation_id"] = serde_json::json!(parent);
        agent.db.upsert_operation(&op).unwrap();
        let guard = acquire(&agent, &provider, &master, child).await.unwrap();
        provider.fail_delete.store(true, Ordering::SeqCst);
        guard.finish(Ok(())).await.unwrap();
        assert_eq!(admission(&agent, parent)["status"], "CLEANUP_REQUIRED");
        assert_eq!(admission(&agent, parent)["operation_id"], child.to_string());
        assert_eq!(
            agent.db.get_operation(child).unwrap().unwrap().state,
            "COMPLETED"
        );
        provider.fail_delete.store(false, Ordering::SeqCst);
        acquire(&agent, &provider, &master, Uuid::new_v4())
            .await
            .unwrap()
            .release()
            .await
            .unwrap();
        assert_eq!(admission(&agent, parent)["status"], "RECOVERED");
    }
    drop(credentials);
    assert!(!config.exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn backup_and_seal_stay_nonterminal_until_release_attempt_returns() {
    use std::time::Duration;
    for kind in ["backup", "seal"] {
        let root = crate::index_test_support::root("gated-backup-seal-completion");
        let agent =
            Arc::new(StateAgent::boot(crate::AgentConfig::isolated(root.join("agent"))).unwrap());
        let app_id = AppId::desktop("gated-finalize");
        if kind == "backup" {
            let install = agent.config.home.join("Games/GatedFinalize");
            std::fs::create_dir_all(&install).unwrap();
            let executable = install.join("game.bin");
            std::fs::write(&executable, b"durable backup fixture").unwrap();
            let mut app = AppIdentity::new(app_id.clone(), "Gated Finalize");
            app.canonical_executable = Some(executable);
            agent.db.upsert_app(&app).unwrap();
            agent
                .db
                .add_known_root(&app_id, "install", install.to_str().unwrap())
                .unwrap();
            agent.db.mark_dirty(&app_id, None, true).unwrap();
        }
        let id = Uuid::new_v4();
        let master = MasterKey::generate();
        let session = crate::index_test_support::session(&root.join("cloud"));
        let (config, credentials) =
            write_guarded_ephemeral_session(&agent.config.paths.run_root, &session).unwrap();
        let mut provider = CredentialsProvider::new(root.join("cloud"), config.clone());
        let started = Arc::new(tokio::sync::Notify::new());
        let resume = Arc::new(tokio::sync::Notify::new());
        provider.release_started = Some(started.clone());
        provider.release_continue = Some(resume.clone());
        let task_agent = agent.clone();
        let (done, result) = tokio::sync::oneshot::channel();
        assert!(agent.operations.spawn_cancellable_on_lane(
            id,
            crate::operation_manager::OperationLane::Transfer,
            async move {
                let _credentials = credentials;
                let result = if kind == "backup" {
                    crate::backup::run_backup(
                        &task_agent,
                        &app_id,
                        BackupMode::CompleteApplication,
                        BackupPerformanceMode::Balanced,
                        &provider,
                        &master,
                        Some(id),
                    )
                    .await
                    .map(|_| ())
                } else {
                    crate::seal::run_seal_for_operation(
                        &task_agent,
                        &provider,
                        &master,
                        BackupMode::CompleteApplication,
                        Some(id),
                    )
                    .await
                    .map(|_| ())
                };
                let _ = done.send(result);
            },
            || panic!("durable finalization must drain instead of cancelling"),
            |_| panic!("durable finalization must not panic"),
        ));
        tokio::time::timeout(Duration::from_secs(10), started.notified())
            .await
            .unwrap();
        let finalizing = if kind == "backup" {
            "COMMITTING"
        } else {
            "COMMITTING_SEAL"
        };
        let active = agent.db.get_operation(id).unwrap().unwrap();
        assert_eq!(active.state, finalizing);
        assert_eq!(
            active.detail_json["completion"]["status"],
            "FINALIZING_REPOSITORY"
        );
        assert_eq!(
            agent.db.get_operation_progress(id).unwrap().unwrap().phase,
            if kind == "backup" {
                "releasing_repository"
            } else {
                finalizing
            }
        );
        if kind == "seal" {
            assert!(
                agent.db.latest_seal().unwrap().is_none(),
                "automatic deletion must not see an early seal"
            );
        }
        assert!(config.is_file());
        agent.operations.cancel(id);
        tokio::task::yield_now().await;
        assert!(agent.operations.is_running(id));
        assert_eq!(
            agent.db.get_operation(id).unwrap().unwrap().state,
            finalizing
        );
        resume.notify_one();
        tokio::time::timeout(Duration::from_secs(10), result)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while agent.operations.is_running(id) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let completed = agent.db.get_operation(id).unwrap().unwrap();
        assert_eq!(
            completed.state,
            if kind == "backup" {
                "COMPLETED"
            } else {
                "SEALED"
            }
        );
        assert_eq!(
            completed.detail_json["repository_admission"]["status"],
            "RELEASED"
        );
        assert!(!config.parent().unwrap().exists());
        if kind == "seal" {
            assert!(agent.db.latest_seal().unwrap().is_some());
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn actual_backup_seal_and_deletion_finalize_with_live_credentials_and_separate_cleanup_outcome(
) {
    let root = crate::index_test_support::root("instance-backup-seal-release");
    let agent = StateAgent::boot(crate::AgentConfig::isolated(root.join("agent"))).unwrap();
    let install = agent.config.home.join("Games/ReleaseFixture");
    std::fs::create_dir_all(&install).unwrap();
    let executable = install.join("game.bin");
    std::fs::write(&executable, b"local backup and seal fixture").unwrap();
    let mut app = AppIdentity::new(AppId::desktop("release-fixture"), "Release Fixture");
    app.canonical_executable = Some(executable);
    agent.db.upsert_app(&app).unwrap();
    agent
        .db
        .add_known_root(&app.app_id, "install", install.to_str().unwrap())
        .unwrap();
    agent.db.mark_dirty(&app.app_id, None, true).unwrap();
    let master = MasterKey::generate();
    let session = crate::index_test_support::session(&root.join("cloud"));
    let (config, credentials) =
        write_guarded_ephemeral_session(&agent.config.paths.run_root, &session).unwrap();
    let provider = CredentialsProvider::new(root.join("cloud"), config.clone());
    let id = Uuid::new_v4();
    provider.fail_delete.store(true, Ordering::SeqCst);
    let manifest = crate::backup::run_backup(
        &agent,
        &app.app_id,
        BackupMode::CompleteApplication,
        BackupPerformanceMode::Balanced,
        &provider,
        &master,
        Some(id),
    )
    .await
    .unwrap();
    assert_eq!(manifest.files.len(), 1);
    assert_eq!(
        agent.db.get_operation(id).unwrap().unwrap().state,
        "COMPLETED"
    );
    assert_eq!(admission(&agent, id)["status"], "CLEANUP_REQUIRED");
    provider.fail_delete.store(false, Ordering::SeqCst);
    acquire(&agent, &provider, &master, Uuid::new_v4())
        .await
        .unwrap()
        .release()
        .await
        .unwrap();
    provider.fail_delete.store(true, Ordering::SeqCst);
    let seal = crate::seal::run_seal(&agent, &provider, &master, BackupMode::CompleteApplication)
        .await
        .unwrap();
    assert_eq!(seal.state, "complete");
    let sealed = agent
        .db
        .recent_operations(20)
        .unwrap()
        .into_iter()
        .find(|op| op.kind == "seal")
        .unwrap();
    assert_eq!(sealed.state, "SEALED");
    assert_eq!(
        admission(&agent, sealed.operation_id)["status"],
        "CLEANUP_REQUIRED"
    );
    provider.fail_delete.store(false, Ordering::SeqCst);
    acquire(&agent, &provider, &master, Uuid::new_v4())
        .await
        .unwrap()
        .release()
        .await
        .unwrap();
    let deletion_id = Uuid::new_v4();
    let full = prepare_full_deletion_selected(
        &provider,
        &master,
        &[(app.app_id.clone(), manifest.bundle_id)],
        deletion_id,
        &owner(&agent, &master).unwrap(),
    )
    .await
    .unwrap();
    operation(&agent, deletion_id, "delete", "RUNNING");
    let plan = crate::deletion::DeletionPlan::from_full(full, "local-fixture".into());
    provider.fail_delete.store(true, Ordering::SeqCst);
    let before = provider.delete_calls.load(Ordering::SeqCst);
    plan.run(&agent, Some(&provider), Some(&master))
        .await
        .unwrap();
    assert_eq!(provider.delete_calls.load(Ordering::SeqCst), before + 1);
    assert_eq!(admission(&agent, deletion_id)["status"], "CLEANUP_REQUIRED");
    assert!(pending_deletions(&provider, &master)
        .await
        .unwrap()
        .is_empty());
    assert!(
        read_committed_manifest(&provider, &master, &app.app_id, manifest.bundle_id)
            .await
            .is_err()
    );
    assert!(config.is_file());
    drop(credentials);
    assert!(!config.exists());
    std::fs::remove_dir_all(root).unwrap();
}
