//! Small, credential-free stage reports. Reporting is never a publication barrier
//! and must not replace a transfer error or advertise durable success.
use std::time::Instant;

use chrono::Utc;
use noland_state_core::{OperationProgress, Result};
use noland_storage::{MetadataReadMetrics, SharedStorageProvider, StorageOperationMetrics};
use serde_json::json;
use uuid::Uuid;

use crate::StateAgent;

#[derive(Clone, Copy)]
pub(crate) enum Stage {
    Manifest,
    Catalog,
    History,
    MetadataCache,
    Index,
    Checkpoint,
    RepositoryRelease,
}

impl Stage {
    fn phase(self) -> &'static str {
        match self {
            Self::Manifest => "publishing_manifest",
            Self::Catalog => "publishing_catalog",
            Self::History => "verifying_backup_history",
            Self::MetadataCache => "caching_backup_metadata",
            Self::Index => "saving_backup_index",
            Self::Checkpoint => "writing_local_checkpoint",
            Self::RepositoryRelease => "releasing_repository",
        }
    }

    fn message(self) -> &'static str {
        match self {
            Self::Manifest => "Publishing and verifying backup objects",
            Self::Catalog => "Publishing the current backup catalog",
            Self::History => "Verifying and safely converging backup history",
            Self::MetadataCache => "Saving verified backup metadata locally",
            Self::Index => "Saving the captured file index",
            Self::Checkpoint => "Writing the local learned-state checkpoint",
            Self::RepositoryRelease => "Data committed; verifying repository claim cleanup",
        }
    }
}

pub(crate) struct Step<'a> {
    agent: &'a StateAgent,
    provider: &'a dyn SharedStorageProvider,
    operation: Uuid,
    stage: Stage,
    started: Instant,
    started_at: String,
    io_before: StorageOperationMetrics,
    reads_before: MetadataReadMetrics,
    finished: bool,
}

impl<'a> Step<'a> {
    pub(crate) fn begin(
        agent: &'a StateAgent,
        provider: &'a dyn SharedStorageProvider,
        operation: Uuid,
        stage: Stage,
    ) -> Self {
        let step = Self {
            agent,
            provider,
            operation,
            stage,
            started: Instant::now(),
            started_at: Utc::now().to_rfc3339(),
            io_before: provider.operation_metrics(),
            reads_before: provider.metadata_read_metrics(),
            finished: false,
        };
        step.report(step.begin_report());
        step
    }

    fn begin_report(&self) -> Result<()> {
        let mut progress = self
            .agent
            .db
            .get_operation_progress(self.operation)?
            .unwrap_or_else(|| OperationProgress::new(self.stage.phase(), 0));
        progress.phase = self.stage.phase().into();
        progress.message = Some(self.stage.message().into());
        // Capture/pack counters remain in detail_json. They are not a percentage
        // of metadata validation, history maintenance or repository release.
        progress.completed_units = 0;
        progress.total_units = None;
        progress.unit = None;
        progress.updated_at = Utc::now();
        if !progress.detail_json.is_object() {
            progress.detail_json = json!({});
        }
        let details = &mut progress.detail_json;
        if !details["finalization"].is_object() {
            details["finalization"] = json!({"version": 1, "stages": {}});
        }
        let finalization = &mut details["finalization"];
        finalization["version"] = json!(1);
        finalization["stage"] = json!(self.stage.phase());
        finalization["started_at"] = json!(self.started_at);
        if !finalization["stages"].is_object() {
            finalization["stages"] = json!({});
        }
        finalization["stages"][self.stage.phase()] = json!({
            "started_at": self.started_at,
            "outcome": "running"
        });
        if let Some(pipeline) = details["streaming_pipeline"].as_object_mut() {
            pipeline.insert("waiting_on".into(), json!(self.stage.phase()));
        }
        self.agent
            .db
            .set_operation_progress(self.operation, Some(&progress))
    }

    pub(crate) fn finish(mut self) {
        self.finished = true;
        self.report(self.finish_report("completed"));
    }

    pub(crate) fn finish_repository_release(mut self) {
        self.finished = true;
        let outcome = match self.agent.db.get_operation(self.operation) {
            Ok(Some(operation))
                if operation.detail_json["repository_admission"]["status"] == "RELEASED" =>
            {
                "completed"
            }
            Ok(Some(operation))
                if operation.detail_json["repository_admission"]["cleanup_required"] == true =>
            {
                "cleanup_required"
            }
            _ => "unconfirmed",
        };
        self.report(self.finish_report(outcome));
    }

    fn finish_report(&self, outcome: &str) -> Result<()> {
        let Some(mut progress) = self.agent.db.get_operation_progress(self.operation)? else {
            // Never resurrect progress cleared by authoritative completion.
            return Ok(());
        };
        let Some(stage) = progress
            .detail_json
            .get_mut("finalization")
            .and_then(|value| value.get_mut("stages"))
            .and_then(|value| value.get_mut(self.stage.phase()))
        else {
            return Ok(());
        };
        if !stage.is_object()
            || stage.get("started_at").and_then(serde_json::Value::as_str) != Some(&self.started_at)
        {
            // A retry/newer report owns this slot; its data must not be replaced.
            return Ok(());
        }
        stage["elapsed_ms"] =
            json!(self.started.elapsed().as_millis().min(u64::MAX as u128) as u64);
        stage["outcome"] = json!(outcome);
        stage["storage_io"] = json!(self
            .provider
            .operation_metrics()
            .saturating_sub(self.io_before));
        stage["metadata_reads"] = json!(self
            .provider
            .metadata_read_metrics()
            .saturating_sub(self.reads_before));
        progress.updated_at = Utc::now();
        self.agent
            .db
            .set_operation_progress(self.operation, Some(&progress))
    }

    fn report(&self, result: Result<()>) {
        if result.is_err() {
            // Do not copy provider stderr, keys or request data into telemetry.
            tracing::warn!(
                operation_id = %self.operation,
                stage = self.stage.phase(),
                "could not persist backup finalization stage diagnostics"
            );
        }
    }
}

impl Drop for Step<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.report(self.finish_report("failed"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index_test_support::TestProvider;
    use noland_crypto::MasterKey;
    use noland_state_core::{
        AppId, AppIdentity, BackupMode, BackupPerformanceMode, OperationRecord, StateError,
    };
    use noland_storage::{Health, LocalStorage, RemoteEntry, RemoteKey, RemoteMeta};
    use std::{
        path::Path,
        sync::{
            atomic::{AtomicU64, Ordering},
            Arc,
        },
        time::Duration,
    };

    fn fixture() -> (std::path::PathBuf, StateAgent, LocalStorage, Uuid) {
        let root = std::env::temp_dir().join(format!("finalization-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let agent = StateAgent::boot(crate::AgentConfig::isolated(root.clone())).unwrap();
        let provider = LocalStorage::new(root.join("provider"));
        let id = Uuid::new_v4();
        let now = Utc::now();
        agent
            .db
            .upsert_operation(&OperationRecord {
                operation_id: id,
                kind: "backup".into(),
                app_id: None,
                state: "COMMITTING".into(),
                created_at: now,
                updated_at: now,
                last_error: None,
                detail_json: json!({}),
            })
            .unwrap();
        let mut progress = OperationProgress::new("committing", 100);
        progress.total_units = Some(100);
        progress.detail_json = json!({
            "streaming_pipeline":{"capture_complete":true,"selected_files":7,
                "prepared_bytes":0,"waiting_on":"manifest_commit"},
            "completed_pack_bytes":1234
        });
        agent
            .db
            .set_operation_progress(id, Some(&progress))
            .unwrap();
        (root, agent, provider, id)
    }

    #[test]
    fn finalization_reports_stages_without_percentages_or_early_success() {
        let (root, agent, provider, id) = fixture();
        Step::begin(&agent, &provider, id, Stage::History).finish();
        let progress = agent.db.get_operation_progress(id).unwrap().unwrap();
        assert_eq!(progress.phase, "verifying_backup_history");
        assert_eq!(progress.fraction(), None);
        assert_eq!(progress.detail_json["completed_pack_bytes"], 1234);
        assert_eq!(
            progress.detail_json["streaming_pipeline"]["selected_files"],
            7
        );
        assert_eq!(
            progress.detail_json["streaming_pipeline"]["waiting_on"],
            "verifying_backup_history"
        );
        let stage = &progress.detail_json["finalization"]["stages"]["verifying_backup_history"];
        assert_eq!(stage["outcome"], "completed");
        assert!(stage["elapsed_ms"].as_u64().is_some());
        assert_eq!(stage["storage_io"]["bytes_uploaded"], 0);
        let operation = agent.db.get_operation(id).unwrap().unwrap();
        assert_eq!(operation.state, "COMMITTING");
        assert!(operation.detail_json.get("completion").is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_and_superseded_steps_keep_newer_phase_and_diagnostics() {
        let (root, agent, provider, id) = fixture();
        let old = Step::begin(&agent, &provider, id, Stage::Manifest);
        Step::begin(&agent, &provider, id, Stage::Catalog).finish();
        drop(old);
        let progress = agent.db.get_operation_progress(id).unwrap().unwrap();
        assert_eq!(progress.phase, "publishing_catalog");
        assert_eq!(
            progress.detail_json["finalization"]["stages"]["publishing_manifest"]["outcome"],
            "failed"
        );
        assert_eq!(
            progress.detail_json["finalization"]["stages"]["publishing_catalog"]["outcome"],
            "completed"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reports_survive_completion_and_retry_does_not_reuse_old_timings() {
        let (root, agent, provider, id) = fixture();
        Step::begin(&agent, &provider, id, Stage::Checkpoint).finish();
        let mut operation = agent.db.get_operation(id).unwrap().unwrap();
        crate::operation_diagnostics::retain(&agent, &mut operation).unwrap();
        assert_eq!(
            operation.detail_json["finalization"]["stage"],
            "writing_local_checkpoint"
        );
        crate::operation_diagnostics::begin_retry(&mut operation);
        assert!(operation.detail_json.get("finalization").is_none());
        assert!(operation.detail_json["previous_attempt_diagnostics"]["finalization"].is_object());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn release_reporting_distinguishes_cleanup_warning_and_never_recreates_cleared_progress() {
        let (root, agent, provider, id) = fixture();
        let step = Step::begin(&agent, &provider, id, Stage::RepositoryRelease);
        let mut operation = agent.db.get_operation(id).unwrap().unwrap();
        operation.detail_json["repository_admission"] =
            json!({"status":"CLEANUP_REQUIRED","cleanup_required":true});
        agent.db.upsert_operation(&operation).unwrap();
        step.finish_repository_release();
        let progress = agent.db.get_operation_progress(id).unwrap().unwrap();
        assert_eq!(
            progress.detail_json["finalization"]["stages"]["releasing_repository"]["outcome"],
            "cleanup_required"
        );
        let step = Step::begin(&agent, &provider, id, Stage::Index);
        agent.db.set_operation_progress(id, None).unwrap();
        step.finish();
        assert!(agent.db.get_operation_progress(id).unwrap().is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn malformed_or_replaced_reports_cannot_panic_or_overwrite_newer_progress() {
        let (root, agent, provider, id) = fixture();
        let step = Step::begin(&agent, &provider, id, Stage::Catalog);
        let mut progress = agent.db.get_operation_progress(id).unwrap().unwrap();
        progress.detail_json["finalization"] = json!(42);
        agent
            .db
            .set_operation_progress(id, Some(&progress))
            .unwrap();
        step.finish();
        assert_eq!(
            agent
                .db
                .get_operation_progress(id)
                .unwrap()
                .unwrap()
                .detail_json["finalization"],
            42
        );
        let old = Step::begin(&agent, &provider, id, Stage::Index);
        let mut progress = agent.db.get_operation_progress(id).unwrap().unwrap();
        progress.detail_json["finalization"]["stages"]["saving_backup_index"] =
            json!({"started_at":"newer-report","outcome":"running"});
        agent
            .db
            .set_operation_progress(id, Some(&progress))
            .unwrap();
        old.finish();
        assert_eq!(
            agent
                .db
                .get_operation_progress(id)
                .unwrap()
                .unwrap()
                .detail_json["finalization"]["stages"]["saving_backup_index"]["outcome"],
            "running"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    struct BackupProbe {
        inner: TestProvider,
        db: noland_state_db::StateDb,
        operation: Uuid,
        pause_history: bool,
        pause_release: bool,
        fail_release: bool,
        fail_manifest: bool,
        replace_failed_report: bool,
        history_started: tokio::sync::Notify,
        history_continue: tokio::sync::Notify,
        release_started: tokio::sync::Notify,
        release_continue: tokio::sync::Notify,
        stats: AtomicU64,
    }

    impl BackupProbe {
        fn new(agent: &StateAgent, inner: TestProvider, operation: Uuid) -> Self {
            Self {
                inner,
                db: agent.db.clone(),
                operation,
                pause_history: false,
                pause_release: false,
                fail_release: false,
                fail_manifest: false,
                replace_failed_report: false,
                history_started: tokio::sync::Notify::new(),
                history_continue: tokio::sync::Notify::new(),
                release_started: tokio::sync::Notify::new(),
                release_continue: tokio::sync::Notify::new(),
                stats: AtomicU64::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl SharedStorageProvider for BackupProbe {
        async fn health_check(&self) -> Result<Health> {
            self.inner.health_check().await
        }

        async fn ensure_root(&self) -> Result<()> {
            self.inner.ensure_root().await
        }

        async fn stat(&self, key: &RemoteKey) -> Result<Option<RemoteMeta>> {
            self.stats.fetch_add(1, Ordering::Relaxed);
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
            if self.pause_history && key.as_str().starts_with("bundles/") {
                self.history_started.notify_one();
                self.history_continue.notified().await;
            }
            self.inner.list_prefix(key).await
        }

        async fn put_small_versioned(
            &self,
            bytes: bytes::Bytes,
            key: &RemoteKey,
        ) -> Result<RemoteMeta> {
            if self.fail_manifest && key.as_str().ends_with("/manifest.enc") {
                if self.replace_failed_report {
                    let mut progress = self
                        .db
                        .get_operation_progress(self.operation)?
                        .expect("manifest stage must have started");
                    progress.detail_json["finalization"] = json!(42);
                    self.db
                        .set_operation_progress(self.operation, Some(&progress))?;
                }
                return Err(StateError::Integrity(
                    "original manifest publication failure".into(),
                ));
            }
            self.inner.put_small_versioned(bytes, key).await
        }

        async fn delete_object(&self, key: &RemoteKey) -> Result<()> {
            if key.as_str().starts_with("maintenance/activity/") {
                if self.pause_release {
                    self.release_started.notify_one();
                    self.release_continue.notified().await;
                }
                if self.fail_release {
                    return Err(StateError::Storage("private cleanup error fixture".into()));
                }
            }
            self.inner.delete_object(key).await
        }

        async fn remove_empty_bundle(&self, app: &AppId, bundle: Uuid) -> Result<()> {
            self.inner.remove_empty_bundle(app, bundle).await
        }

        fn storage_identity(&self) -> Option<noland_rclone_adapter::ProviderRootIdentity> {
            self.inner.storage_identity()
        }

        fn operation_metrics(&self) -> StorageOperationMetrics {
            StorageOperationMetrics {
                remote_stat_calls: self.stats.load(Ordering::Relaxed),
                ..Default::default()
            }
        }

        fn metadata_read_metrics(&self) -> MetadataReadMetrics {
            let reads = self.inner.controls.reads.lock().len() as u64;
            MetadataReadMetrics {
                read_calls: reads,
                attempts: reads,
                ..Default::default()
            }
        }
    }

    fn backup_fixture(
        label: &str,
    ) -> (
        std::path::PathBuf,
        Arc<StateAgent>,
        TestProvider,
        AppId,
        MasterKey,
    ) {
        let root = crate::index_test_support::root(label);
        let agent =
            Arc::new(StateAgent::boot(crate::AgentConfig::isolated(root.join("agent"))).unwrap());
        let install = agent.config.home.join("Games/Finalization");
        std::fs::create_dir_all(&install).unwrap();
        let executable = install.join("game.bin");
        std::fs::write(&executable, b"one reusable finalization fixture chunk").unwrap();
        let app = AppId::desktop("finalization-integration");
        let mut identity = AppIdentity::new(app.clone(), "Finalization Integration");
        identity.canonical_executable = Some(executable);
        agent.db.upsert_app(&identity).unwrap();
        agent
            .db
            .add_known_root(&app, "install", install.to_str().unwrap())
            .unwrap();
        agent.db.mark_dirty(&app, None, true).unwrap();
        let provider = TestProvider::new(root.join("cloud"));
        (root, agent, provider, app, MasterKey::generate())
    }

    fn assert_all_stages_archived(operation: &OperationRecord, release_outcome: &str) {
        let finalization = &operation.detail_json["finalization"];
        assert_eq!(finalization["stage"], Stage::RepositoryRelease.phase());
        for stage in [
            Stage::Manifest,
            Stage::Catalog,
            Stage::History,
            Stage::MetadataCache,
            Stage::Index,
            Stage::Checkpoint,
            Stage::RepositoryRelease,
        ] {
            let report = &finalization["stages"][stage.phase()];
            assert_eq!(
                report["outcome"],
                if stage.phase() == Stage::RepositoryRelease.phase() {
                    release_outcome
                } else {
                    "completed"
                },
                "{}",
                stage.phase()
            );
            assert!(report["elapsed_ms"].as_u64().is_some(), "{}", stage.phase());
            assert!(report["started_at"].as_str().is_some(), "{}", stage.phase());
            assert!(report["storage_io"].is_object(), "{}", stage.phase());
            assert!(report["metadata_reads"].is_object(), "{}", stage.phase());
        }
        assert_eq!(operation.state, "COMPLETED");
        assert_eq!(operation.detail_json["completion"]["durable"], true);
        assert_eq!(operation.detail_json["completion"]["status"], "FINALIZED");
        assert_eq!(
            operation.detail_json["streaming_pipeline"]["waiting_on"],
            "complete"
        );
    }

    #[tokio::test]
    async fn actual_full_reuse_backup_archives_stages_only_after_authoritative_release() {
        let (root, agent, inner, app, master) = backup_fixture("finalization-reuse-release");
        super::super::run_backup(
            &agent,
            &app,
            BackupMode::CompleteApplication,
            BackupPerformanceMode::Full,
            &inner,
            &master,
            Some(Uuid::new_v4()),
        )
        .await
        .unwrap();
        let operation = Uuid::new_v4();
        // RPC submission persists this row before acquiring repository access.
        let now = Utc::now();
        agent
            .db
            .upsert_operation(&OperationRecord {
                operation_id: operation,
                kind: "backup".into(),
                app_id: Some(app.clone()),
                state: "QUEUED".into(),
                created_at: now,
                updated_at: now,
                last_error: None,
                detail_json: json!({}),
            })
            .unwrap();
        let mut provider = BackupProbe::new(&agent, inner, operation);
        provider.pause_release = true;
        let mut backup = Box::pin(super::super::run_backup(
            &agent,
            &app,
            BackupMode::CompleteApplication,
            BackupPerformanceMode::Full,
            &provider,
            &master,
            Some(operation),
        ));
        tokio::time::timeout(Duration::from_secs(10), async {
            tokio::select! {
                _ = &mut backup => panic!("backup must wait for its claim release"),
                _ = provider.release_started.notified() => {}
            }
        })
        .await
        .unwrap();
        let finalizing = agent.db.get_operation(operation).unwrap().unwrap();
        assert_eq!(finalizing.state, "COMMITTING");
        assert_eq!(finalizing.detail_json["completion"]["durable"], true);
        assert_eq!(
            finalizing.detail_json["completion"]["status"],
            "FINALIZING_REPOSITORY"
        );
        assert_eq!(
            finalizing.detail_json["repository_admission"]["status"],
            "ADMITTED"
        );
        let progress = agent.db.get_operation_progress(operation).unwrap().unwrap();
        assert_eq!(progress.phase, Stage::RepositoryRelease.phase());
        assert_eq!(progress.fraction(), None);
        assert_eq!(
            progress.detail_json["streaming_pipeline"]["waiting_on"],
            "releasing_repository"
        );
        assert_eq!(
            progress.detail_json["finalization"]["stages"]["releasing_repository"]["outcome"],
            "running"
        );
        assert!(
            crate::operation_diagnostics::complete_after_repository_release(&agent, operation)
                .is_err()
        );
        assert!(
            crate::repository::acquire(&agent, &provider, &master, Uuid::new_v4())
                .await
                .is_err()
        );
        provider.release_continue.notify_one();
        let manifest = tokio::time::timeout(Duration::from_secs(10), backup)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(manifest.files.len(), 1);
        let completed = agent.db.get_operation(operation).unwrap().unwrap();
        assert_all_stages_archived(&completed, "completed");
        let metrics = agent.db.get_operation_metrics(operation).unwrap().unwrap();
        assert_eq!(metrics.num_files_rehashed, 1);
        assert_eq!(metrics.num_chunks_reused, 1);
        assert_eq!(metrics.num_chunks_created, 0);
        assert_eq!(metrics.bytes_packed, 0);
        let stages = &completed.detail_json["finalization"]["stages"];
        assert_eq!(
            stages["releasing_repository"]["storage_io"]["remote_stat_calls"],
            2
        );
        assert_eq!(
            stages["releasing_repository"]["metadata_reads"]["read_calls"],
            1
        );
        assert!(
            stages["publishing_catalog"]["metadata_reads"]["read_calls"]
                .as_u64()
                .unwrap()
                > 0
        );
        assert!(agent
            .db
            .get_operation_progress(operation)
            .unwrap()
            .is_none());
        assert!(noland_storage::repository_activities(&provider, &master)
            .await
            .unwrap()
            .is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn actual_backup_cleanup_warning_keeps_durable_success_and_all_stage_reports() {
        let (root, agent, inner, app, master) = backup_fixture("finalization-cleanup-warning");
        let operation = Uuid::new_v4();
        let mut provider = BackupProbe::new(&agent, inner, operation);
        provider.fail_release = true;
        let manifest = super::super::run_backup(
            &agent,
            &app,
            BackupMode::CompleteApplication,
            BackupPerformanceMode::Full,
            &provider,
            &master,
            Some(operation),
        )
        .await
        .unwrap();
        let completed = agent.db.get_operation(operation).unwrap().unwrap();
        assert_all_stages_archived(&completed, "cleanup_required");
        assert_eq!(
            completed.detail_json["repository_admission"]["status"],
            "CLEANUP_REQUIRED"
        );
        assert_eq!(
            completed.detail_json["repository_admission"]["cleanup_required"],
            true
        );
        assert!(!completed
            .detail_json
            .to_string()
            .contains("private cleanup"));
        assert!(completed.last_error.is_none());
        let progress = agent.db.get_operation_progress(operation).unwrap().unwrap();
        assert_eq!(progress.phase, "COMPLETED");
        assert_eq!(
            progress.detail_json["finalization"],
            completed.detail_json["finalization"]
        );
        assert_eq!(
            noland_storage::read_committed_manifest(&provider, &master, &app, manifest.bundle_id)
                .await
                .unwrap()
                .commit_id,
            manifest.commit_id
        );
        assert_eq!(
            noland_storage::repository_activities(&provider, &master)
                .await
                .unwrap()
                .len(),
            1
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn actual_noop_backup_reports_history_and_includes_it_in_final_timings() {
        let (root, agent, inner, app, master) = backup_fixture("finalization-noop-history");
        let original = super::super::run_backup(
            &agent,
            &app,
            BackupMode::CompleteApplication,
            BackupPerformanceMode::Full,
            &inner,
            &master,
            Some(Uuid::new_v4()),
        )
        .await
        .unwrap();
        let operation = Uuid::new_v4();
        let mut provider = BackupProbe::new(&agent, inner, operation);
        provider.pause_history = true;
        let mut backup = Box::pin(super::super::run_backup(
            &agent,
            &app,
            BackupMode::CompleteApplication,
            BackupPerformanceMode::Balanced,
            &provider,
            &master,
            Some(operation),
        ));
        tokio::time::timeout(Duration::from_secs(10), async {
            tokio::select! {
                _ = &mut backup => panic!("no-op history verification must still be awaited"),
                _ = provider.history_started.notified() => {}
            }
        })
        .await
        .unwrap();
        let active = agent.db.get_operation(operation).unwrap().unwrap();
        assert_eq!(active.state, "COMMITTING");
        assert_ne!(active.detail_json["completion"]["durable"], true);
        let progress = agent.db.get_operation_progress(operation).unwrap().unwrap();
        assert_eq!(progress.phase, Stage::History.phase());
        assert_eq!(
            progress.detail_json["finalization"]["stages"][Stage::History.phase()]["outcome"],
            "running"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
        provider.history_continue.notify_one();
        let returned = tokio::time::timeout(Duration::from_secs(10), backup)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(returned.bundle_id, original.bundle_id);
        let completed = agent.db.get_operation(operation).unwrap().unwrap();
        assert_eq!(completed.state, "COMPLETED");
        assert_eq!(completed.detail_json["no_op"], true);
        let stages = &completed.detail_json["finalization"]["stages"];
        assert_eq!(stages.as_object().unwrap().len(), 2);
        let history = &stages[Stage::History.phase()];
        assert_eq!(history["outcome"], "completed");
        assert_eq!(
            stages[Stage::RepositoryRelease.phase()]["outcome"],
            "completed"
        );
        let history_ms = history["elapsed_ms"].as_u64().unwrap();
        assert!(history_ms >= 20);
        let metrics = agent.db.get_operation_metrics(operation).unwrap().unwrap();
        assert!(metrics.commit_duration_ms >= history_ms);
        assert!(metrics.total_duration_ms >= metrics.commit_duration_ms);
        assert_eq!(metrics.num_files_rehashed, 0);
        assert!(agent
            .db
            .get_operation_progress(operation)
            .unwrap()
            .is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn actual_publication_failure_keeps_original_error_when_report_is_intact_or_malformed() {
        for replace_report in [false, true] {
            let (root, agent, inner, app, master) = backup_fixture("finalization-original-error");
            let operation = Uuid::new_v4();
            let mut provider = BackupProbe::new(&agent, inner, operation);
            provider.fail_manifest = true;
            provider.replace_failed_report = replace_report;
            let error = super::super::run_backup(
                &agent,
                &app,
                BackupMode::CompleteApplication,
                BackupPerformanceMode::Full,
                &provider,
                &master,
                Some(operation),
            )
            .await
            .unwrap_err();
            assert!(matches!(error, StateError::Integrity(_)));
            assert_eq!(
                error.to_string(),
                "integrity: original manifest publication failure"
            );
            let mut failed = agent.db.get_operation(operation).unwrap().unwrap();
            assert_ne!(failed.state, "COMPLETED");
            assert_ne!(failed.detail_json["completion"]["durable"], true);
            assert_eq!(
                failed.detail_json["repository_admission"]["status"],
                "RELEASED"
            );
            crate::operation_diagnostics::retain(&agent, &mut failed).unwrap();
            if replace_report {
                assert_eq!(failed.detail_json["finalization"], 42);
            } else {
                assert_eq!(
                    failed.detail_json["finalization"]["stages"]["publishing_manifest"]["outcome"],
                    "failed"
                );
                assert!(
                    failed.detail_json["finalization"]["stages"]["publishing_manifest"]
                        ["elapsed_ms"]
                        .as_u64()
                        .is_some()
                );
            }
            assert!(noland_storage::repository_activities(&provider, &master)
                .await
                .unwrap()
                .is_empty());
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn reporting_write_errors_are_best_effort_and_cannot_mask_an_original_result() {
        let (root, agent, provider, _) = fixture();
        let missing = Uuid::new_v4();
        let success = (|| -> Result<()> {
            Step::begin(&agent, &provider, missing, Stage::Index).finish();
            Ok(())
        })();
        assert!(success.is_ok());
        let failure = (|| -> Result<()> {
            let _report = Step::begin(&agent, &provider, missing, Stage::Catalog);
            Err(StateError::Integrity("original transfer error".into()))
        })();
        assert!(matches!(failure, Err(StateError::Integrity(_))));
        assert!(agent.db.get_operation(missing).unwrap().is_none());
        assert!(agent.db.get_operation_progress(missing).unwrap().is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn malformed_stage_containers_and_retry_reset_do_not_resurrect_old_reports() {
        let (root, agent, provider, operation) = fixture();
        for malformed in [
            json!(null),
            json!(42),
            json!([]),
            json!({"finalization": []}),
            json!({"finalization": {"stages": 42}, "streaming_pipeline": false}),
        ] {
            let mut progress = OperationProgress::new("committing", 0);
            progress.detail_json = malformed;
            agent
                .db
                .set_operation_progress(operation, Some(&progress))
                .unwrap();
            Step::begin(&agent, &provider, operation, Stage::Manifest).finish();
            let progress = agent.db.get_operation_progress(operation).unwrap().unwrap();
            assert_eq!(
                progress.detail_json["finalization"]["stages"]["publishing_manifest"]["outcome"],
                "completed"
            );
        }
        let old = Step::begin(&agent, &provider, operation, Stage::Catalog);
        let mut retry = agent.db.get_operation(operation).unwrap().unwrap();
        crate::operation_diagnostics::retain(&agent, &mut retry).unwrap();
        crate::operation_diagnostics::begin_retry(&mut retry);
        retry.state = "QUEUED".into();
        agent.db.upsert_operation(&retry).unwrap();
        // This is the progress reset performed by admitted RPC retries.
        agent
            .db
            .set_operation_progress(operation, Some(&OperationProgress::new("queued", 0)))
            .unwrap();
        drop(old);
        let progress = agent.db.get_operation_progress(operation).unwrap().unwrap();
        assert_eq!(progress.phase, "queued");
        assert!(progress.detail_json.get("finalization").is_none());
        Step::begin(&agent, &provider, operation, Stage::Index).finish();
        let progress = agent.db.get_operation_progress(operation).unwrap().unwrap();
        assert!(progress.detail_json["finalization"]["stages"]
            .get("publishing_catalog")
            .is_none());
        assert_eq!(
            retry.detail_json["previous_attempt_diagnostics"]["finalization"]["stages"]
                ["publishing_catalog"]["outcome"],
            "running"
        );
        agent.db.set_operation_progress(operation, None).unwrap();
        assert_eq!(
            agent.db.get_operation(operation).unwrap().unwrap().state,
            "QUEUED"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
