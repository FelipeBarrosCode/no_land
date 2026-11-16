//! One serialized collector, with bounded background turns and full capture
//! watermarks. No filesystem, SQLite, procfs or checkpoint work runs in the
//! scheduler's Tokio task.

#[cfg(all(test, unix))]
mod proof_tests;

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use noland_attribution::{AttributionEngine, PendingAttributionState};
use noland_observer::{EventQueue, QueueLoss, QueueWatermark};
use noland_state_core::{constants, Result, StateError};

use crate::StateAgent;

pub(crate) const BACKGROUND_EVENT_BATCH: usize = 512;
const FAILED_TURN_BACKOFF: Duration = Duration::from_millis(250);

#[derive(Default)]
pub(crate) struct CollectorState {
    pending: PendingAttributionState,
    reported_loss: Option<u64>,
}

impl CollectorState {
    pub(super) fn require_generation_proofs(&mut self) {
        self.pending.require_generation_proofs();
    }

    pub(super) fn with_engine<T>(
        &mut self,
        engine: AttributionEngine<'_>,
        run: impl FnOnce(&mut AttributionEngine<'_>) -> Result<T>,
    ) -> Result<T> {
        // This guard transfers facts back even during unwinding. Only pending
        // generation proofs survive; authority caches belong to this turn.
        let mut batch = AttributionBatch {
            engine: engine.with_pending_state(std::mem::take(&mut self.pending)),
            pending: &mut self.pending,
        };
        run(&mut batch.engine)
    }
}

struct AttributionBatch<'db, 'pending> {
    engine: AttributionEngine<'db>,
    pending: &'pending mut PendingAttributionState,
}

impl Drop for AttributionBatch<'_, '_> {
    fn drop(&mut self) {
        *self.pending = self.engine.take_pending_state();
    }
}

pub(super) fn process_events(agent: &StateAgent, limit: usize, full_flush: bool) -> Result<usize> {
    let mut collector = agent.collector.lock();
    process_events_collected(agent, &mut collector, limit, full_flush)
}

pub(super) fn process_events_collected(
    agent: &StateAgent,
    collector: &mut CollectorState,
    limit: usize,
    full_flush: bool,
) -> Result<usize> {
    let result: Result<usize> = (|| {
        let (processed, watermark) =
            collector.with_engine(agent.attribution_engine(), |engine| {
                let processed =
                    noland_attribution::process_hub_events_watermark(engine, &agent.hub, limit)?;
                if full_flush {
                    engine.fence_pending()?;
                }
                Ok(processed)
            })?;
        reconcile_loss(&agent.hub.queue, full_flush, Some(watermark), |loss| {
            recover_queue_loss(agent, collector, loss)
        })?;
        Ok(processed)
    })();
    if let Err(error) = &result {
        // A failed durable fence must never become a successful fresh preflight.
        // Ingestion already retains its failed fact and FIFO tail; other errors
        // still need a sticky retry/reconciliation barrier.
        if agent.hub.queue.loss_snapshot().is_none() {
            agent.hub.queue.report_loss(1);
        }
        agent.observer.signal_loss("collector", error.to_string());
    }
    result
}

fn reconcile_loss(
    queue: &EventQueue,
    require_complete: bool,
    watermark: Option<QueueWatermark>,
    reconcile: impl FnOnce(QueueLoss) -> Result<()>,
) -> Result<()> {
    let Some(loss) = queue.loss_snapshot() else {
        return Ok(());
    };
    reconcile(loss)?;
    let acknowledged = match watermark {
        Some(watermark) => {
            queue.acknowledge_watermark_loss(loss, watermark)
                || (!require_complete && queue.acknowledge_loss(loss))
        }
        None => queue.acknowledge_loss(loss),
    };
    if !acknowledged && require_complete {
        return Err(StateError::Invalid(
            "observer loss or an unprocessed suffix remains after reconciliation; retry the flush"
                .into(),
        ));
    }
    Ok(())
}

fn recover_queue_loss(
    agent: &StateAgent,
    collector: &mut CollectorState,
    loss: QueueLoss,
) -> Result<()> {
    // Loss can arrive after the detached prefix was processed. Do not let
    // procfs recovery attach an older unresolved fact to a later PID generation.
    collector.with_engine(agent.attribution_engine(), |engine| {
        engine.observe_loss(loss.dropped);
        engine.fence_pending_after_loss()
    })?;
    let app_ids = agent.db.open_session_app_ids()?;
    for app_id in &app_ids {
        agent.db.mark_dirty(app_id, None, true)?;
        let known_roots = agent.db.known_roots(Some(app_id))?;
        if known_roots.is_empty() {
            agent
                .db
                .mark_dirty_root(app_id, &agent.config.home.to_string_lossy(), None, true)?;
        } else {
            for (_, _, root) in known_roots {
                agent.db.mark_dirty_root(app_id, &root, None, true)?;
            }
        }
        // Preserve one app/root generation barrier, independent of file count.
    }
    let active = app_ids.iter().collect::<std::collections::HashSet<_>>();
    let mut inactive_fenced = 0;
    for app in agent.db.list_apps()? {
        if active.contains(&app.app_id) {
            continue;
        }
        // An origin may have exited before this global loss was consumed. A
        // missing open session is not proof that its old trusted index is clean.
        // Invalidate only the existing app's state: do not invent a HOME fallback
        // or new private roots for an unidentified/ended origin.
        agent.db.mark_dirty(&app.app_id, None, true)?;
        inactive_fenced += 1;
    }
    if collector.reported_loss != Some(loss.dropped) {
        agent.observer.signal_loss(
            "agent_queue",
            format!(
                "{} total events dropped; {} active apps and {} inactive apps fenced for reconciliation",
                loss.dropped,
                app_ids.len(),
                inactive_fenced
            ),
        );
        collector.reported_loss = Some(loss.dropped);
    }
    if agent.hub.queue.has_queued_events() {
        // This prefix is durably fenced, but its FIFO suffix may still straddle
        // a lost process boundary. Keep the loss flag and do not let a newer
        // procfs PID/session snapshot overtake the remaining facts.
        return Ok(());
    }
    let recovery = agent.reconcile_live_processes_collected(collector)?;
    tracing::info!(
        processes_seen = recovery.processes_seen,
        sessions_pruned = recovery.sessions_pruned,
        sessions_recovered = recovery.sessions_recovered,
        open_files_recovered = recovery.open_files_recovered,
        "reconciled live processes immediately after observer loss"
    );
    Ok(())
}

#[derive(Clone, Copy, Debug)]
enum Work {
    Events,
    RecoverProcesses,
    Discover,
    Checkpoint,
}

impl Work {
    fn run(self, agent: &StateAgent) -> Result<()> {
        match self {
            Self::Events => process_events(agent, BACKGROUND_EVENT_BATCH, false).map(|_| ()),
            Self::RecoverProcesses => {
                let mut collector = agent.collector.lock();
                if agent.hub.queue.has_queued_events() {
                    // Background turns stay bounded: primary observation must
                    // catch up before this lower-priority procfs snapshot.
                    return Ok(());
                }
                let recovery = agent.reconcile_live_processes_collected(&mut collector)?;
                if recovery.sessions_pruned > 0 || recovery.sessions_recovered > 0 {
                    tracing::info!(
                        sessions_pruned = recovery.sessions_pruned,
                        sessions_recovered = recovery.sessions_recovered,
                        open_files_recovered = recovery.open_files_recovered,
                        "reconciled application sessions from procfs"
                    );
                }
                Ok(())
            }
            Self::Discover => agent.discover(),
            Self::Checkpoint => {
                let _collector = agent.collector.lock();
                crate::checkpoint::maybe_checkpoint(agent)
            }
        }
    }
}

pub(super) fn spawn_background(agent: &Arc<StateAgent>) {
    if agent.collector_started.swap(true, Ordering::AcqRel) {
        return;
    }
    let agent = Arc::clone(agent);
    tokio::spawn(async move {
        let mut process_recovery = tokio::time::interval(Duration::from_secs(30));
        let mut rediscovery = tokio::time::interval(Duration::from_secs(30));
        let mut checkpoint =
            tokio::time::interval(Duration::from_secs(constants::CHECKPOINT_INTERVAL_SECS));
        // Do not replay a missed interval backlog after a slow blocking turn.
        process_recovery.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        rediscovery.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        checkpoint.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let work = tokio::select! {
                _ = agent.hub.wait_for_events() => Work::Events,
                _ = process_recovery.tick() => Work::RecoverProcesses,
                _ = rediscovery.tick() => Work::Discover,
                _ = checkpoint.tick() => Work::Checkpoint,
            };
            let result = join_work(&agent, move |agent| work.run(agent)).await;
            if let Err(error) = &result {
                tracing::warn!(?work, %error, "background collector turn failed");
            }
            // The job is joined before pacing. This wait is deliberately outside
            // select!: neither a retained suffix nor producer wakeups bypass it.
            pace_after_turn(result.is_err()).await;
            // Queue::notified checks remaining data as well as Notify's permit.
            // A bounded turn therefore cannot strand a suffix without a push.
        }
    });
}

async fn pace_after_turn(failed: bool) {
    if failed {
        // No collector lock or repository claim is held here. Successful bounded
        // prefixes, including a still-pending loss suffix, are rearmed promptly.
        tokio::time::sleep(FAILED_TURN_BACKOFF).await;
    }
}

async fn join_work(
    agent: &Arc<StateAgent>,
    run: impl FnOnce(&StateAgent) -> Result<()> + Send + 'static,
) -> Result<()> {
    let worker = Arc::clone(agent);
    let job = tokio::task::spawn_blocking(move || run(&worker));
    // Never put this JoinHandle inside select! or start a second turn while it
    // is running. The blocking work has no repository claim.
    let result = match job.await {
        Ok(result) => result,
        Err(error) => Err(StateError::Invalid(format!(
            "collector blocking worker failed: {error}"
        ))),
    };
    if let Err(error) = &result {
        agent.hub.queue.report_loss(1);
        agent
            .observer
            .signal_loss("collector_worker", error.to_string());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use noland_observer::{fs_event, process_exec};
    use noland_state_core::metrics::Metrics;
    use noland_state_core::*;
    use noland_storage::{
        Health, LocalStorage, ProviderRootIdentity, RemoteEntry, RemoteKey, RemoteMeta,
        SharedStorageProvider,
    };
    use std::path::Path;
    use std::path::PathBuf;
    use uuid::Uuid;

    struct InterruptedPublication {
        inner: LocalStorage,
        fail_manifest: std::sync::atomic::AtomicBool,
    }

    #[async_trait::async_trait]
    impl SharedStorageProvider for InterruptedPublication {
        async fn health_check(&self) -> Result<Health> {
            self.inner.health_check().await
        }
        async fn ensure_root(&self) -> Result<()> {
            self.inner.ensure_root().await
        }
        async fn stat(&self, key: &RemoteKey) -> Result<Option<RemoteMeta>> {
            self.inner.stat(key).await
        }
        async fn read_small(&self, key: &RemoteKey, limit: u64) -> Result<Vec<u8>> {
            self.inner.read_small(key, limit).await
        }
        async fn upload_immutable(&self, local: &Path, key: &RemoteKey) -> Result<RemoteMeta> {
            self.inner.upload_immutable(local, key).await
        }
        async fn download(&self, key: &RemoteKey, dest: &Path) -> Result<()> {
            self.inner.download(key, dest).await
        }
        async fn list_prefix(&self, key: &RemoteKey) -> Result<Vec<RemoteEntry>> {
            self.inner.list_prefix(key).await
        }
        async fn put_small_versioned(
            &self,
            bytes: bytes::Bytes,
            key: &RemoteKey,
        ) -> Result<RemoteMeta> {
            if key.as_str().ends_with("manifest.enc")
                && self.fail_manifest.swap(false, Ordering::SeqCst)
            {
                return Err(StateError::Storage(
                    "injected interruption after coherent capture".into(),
                ));
            }
            self.inner.put_small_versioned(bytes, key).await
        }
        async fn delete_object(&self, key: &RemoteKey) -> Result<()> {
            self.inner.delete_object(key).await
        }
        fn storage_identity(&self) -> Option<ProviderRootIdentity> {
            self.inner.storage_identity()
        }
    }

    fn fixture(label: &str) -> (PathBuf, StateAgent, AppId, i32, PathBuf) {
        let root = crate::index_test_support::root(label);
        let agent = StateAgent::boot(crate::AgentConfig::isolated(root.clone())).unwrap();
        let app = AppId::desktop("collector-fixture");
        let mut identity = AppIdentity::new(app.clone(), "Collector Fixture");
        identity.canonical_executable = Some(agent.config.home.join("Games/Collector/game"));
        agent.db.upsert_app(&identity).unwrap();
        let save = agent.config.home.join("Games/Collector/save.dat");
        std::fs::create_dir_all(save.parent().unwrap()).unwrap();
        std::fs::write(&save, b"old!").unwrap();
        agent
            .db
            .add_known_root(&app, "install", save.parent().unwrap().to_str().unwrap())
            .unwrap();
        let pid = i32::MAX - 20;
        (root, agent, app, pid, save)
    }

    fn queued_backup(agent: &StateAgent, operation: Uuid, app: &AppId) -> OperationMetrics {
        let metrics = OperationMetrics {
            discovery_duration_ms: 37,
            ..OperationMetrics::default()
        };
        agent
            .db
            .upsert_operation(&OperationRecord {
                operation_id: operation,
                kind: "backup".into(),
                app_id: Some(app.clone()),
                state: BackupState::Queued.as_str().into(),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                last_error: None,
                detail_json: serde_json::json!({"metrics": metrics}),
            })
            .unwrap();
        agent.db.set_operation_metrics(operation, &metrics).unwrap();
        metrics
    }

    fn padding(agent: &StateAgent, count: usize) {
        for index in 0..count {
            let mut event = process_exec(i32::MAX - 1000 - index as i32, 1, "/opt/unused");
            event.kind = ProcessEventKind::Exit;
            agent.hub.inject_process(event);
        }
    }

    #[test]
    fn source_db_errors_and_racing_losses_leave_the_reconciliation_barrier_sticky() {
        let queue = EventQueue::new(4, Arc::new(Metrics::default()));
        queue.report_loss(1);
        assert!(reconcile_loss(&queue, true, None, |_| {
            Err(StateError::Database(
                "injected durable fence failure".into(),
            ))
        })
        .is_err());
        assert!(queue.loss_snapshot().is_some());
        assert!(reconcile_loss(&queue, true, None, |_| {
            queue.report_loss(1);
            Ok(())
        })
        .is_err());
        assert_eq!(queue.loss_snapshot().unwrap().dropped, 2);
        reconcile_loss(&queue, true, None, |_| Ok(())).unwrap();
        assert!(queue.loss_snapshot().is_none());
    }

    #[test]
    fn completed_full_watermarks_do_not_chase_a_continuously_replaced_new_tail() {
        let queue = EventQueue::new(16, Arc::new(Metrics::default()));
        queue.push(noland_observer::QueuedEvent::FilesystemGap { pid: 1 });
        for next in 2..8 {
            queue.report_loss(1);
            let (covered, watermark) = queue.drain_observed_prefix(usize::MAX);
            assert_eq!(covered.len(), 1);
            reconcile_loss(&queue, true, Some(watermark), |_| {
                queue.push(noland_observer::QueuedEvent::FilesystemGap { pid: next });
                Ok(())
            })
            .unwrap();
            assert!(queue.loss_snapshot().is_none());
            assert!(
                queue.has_queued_events(),
                "the post-watermark suffix is not consumed"
            );
        }
        assert!(matches!(
            queue.drain().as_slice(),
            [noland_observer::QueuedEvent::FilesystemGap { pid: 7 }]
        ));
    }

    #[test]
    fn a_new_loss_during_a_full_watermark_is_never_acknowledged_by_the_older_fence() {
        let queue = EventQueue::new(16, Arc::new(Metrics::default()));
        queue.report_loss(1);
        queue.push(noland_observer::QueuedEvent::FilesystemGap { pid: 1 });
        let (_, watermark) = queue.drain_observed_prefix(usize::MAX);
        assert!(reconcile_loss(&queue, true, Some(watermark), |_| {
            queue.push(noland_observer::QueuedEvent::FilesystemGap { pid: 2 });
            queue.report_loss(1);
            Ok(())
        })
        .is_err());
        assert_eq!(queue.loss_snapshot().unwrap().dropped, 2);
        assert!(queue.has_queued_events());
    }

    #[test]
    fn global_loss_fences_ended_app_trust_without_expanding_its_roots() {
        let (root, agent, app, pid, save) = fixture("collector-ended-loss");
        let session = AppSession::new(app.clone(), pid, SessionSource::ExecutableDiscovery);
        agent.db.insert_session(&session).unwrap();
        agent.db.end_session(session.session_id).unwrap();
        let logical = agent.roots.lock().classify(&save).unwrap();
        agent
            .db
            .upsert_file_states(&[FileStateRecord {
                app_id: app.clone(),
                logical_root: logical.logical_root.as_token(),
                canonical_path: Some(save.to_string_lossy().into_owned()),
                relative_path: logical.relative_path,
                file_type: FileType::File,
                size: 4,
                mtime_ns: 1,
                inode: None,
                mount_id: None,
                mode: None,
                content_hash: Some("retained-hash".into()),
                trust: FileStateTrust::Trusted,
                last_seen_at: chrono::Utc::now(),
                last_hashed_at: Some(chrono::Utc::now()),
            }])
            .unwrap();
        let roots = agent.db.known_roots(Some(&app)).unwrap();
        let before = agent.db.capture_generation(&app).unwrap();
        agent.hub.queue.report_loss(1);
        assert_eq!(agent.process_events().unwrap(), 0);
        let indexed = agent.db.list_file_states(&app, None).unwrap();
        assert_eq!(indexed[0].trust, FileStateTrust::VerifyRequired);
        assert_eq!(indexed[0].content_hash.as_deref(), Some("retained-hash"));
        assert_eq!(agent.db.capture_generation(&app).unwrap(), before + 1);
        assert_eq!(agent.db.known_roots(Some(&app)).unwrap(), roots);
        assert!(agent.db.list_dirty_roots(Some(&app)).unwrap().is_empty());
        assert!(agent
            .db
            .list_dirty_apps()
            .unwrap()
            .iter()
            .any(|dirty| { dirty.app_id == app && dirty.requires_reconciliation }));
        assert!(agent.hub.queue.loss_snapshot().is_none());
        assert!(
            agent
                .observer
                .status(agent.hub.queue.dropped())
                .reconciliation_required
        );
        drop(agent);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn full_flush_fences_an_unidentified_private_fact_without_authorizing_new_roots() {
        let (root, agent, app, pid, _) = fixture("collector-unidentified-private");
        let private = agent
            .config
            .home
            .join(".local/share/noland-runtime/app-data/current/.config/save.dat");
        std::fs::create_dir_all(private.parent().unwrap()).unwrap();
        std::fs::write(&private, b"private save").unwrap();
        let roots = agent.db.known_roots(Some(&app)).unwrap();
        let before = agent.db.capture_generation(&app).unwrap();
        agent
            .hub
            .inject_fs(fs_event(FsEventKind::Write, pid, &private));
        assert_eq!(agent.process_events().unwrap(), 1);
        assert_eq!(agent.db.capture_generation(&app).unwrap(), before + 1);
        assert_eq!(agent.db.known_roots(Some(&app)).unwrap(), roots);
        assert!(agent.db.list_dirty_roots(Some(&app)).unwrap().is_empty());
        assert!(agent
            .db
            .list_dirty_apps()
            .unwrap()
            .iter()
            .any(|dirty| { dirty.app_id == app && dirty.requires_reconciliation }));
        assert!(agent.db.associations_for_app(&app).unwrap().is_empty());
        assert_eq!(agent.hub.queue.dropped(), 0);
        drop(agent);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bounded_batches_preserve_pending_facts_and_normal_backpressure_is_not_loss() {
        let (root, agent, app, pid, save) = fixture("collector-pending-batches");
        padding(&agent, BACKGROUND_EVENT_BATCH - 1);
        agent
            .hub
            .inject_fs(fs_event(FsEventKind::Write, pid, &save));
        agent.hub.inject_process(process_exec(
            pid,
            1,
            agent
                .db
                .get_app(&app)
                .unwrap()
                .unwrap()
                .canonical_executable
                .unwrap(),
        ));
        assert_eq!(
            process_events(&agent, BACKGROUND_EVENT_BATCH, false).unwrap(),
            BACKGROUND_EVENT_BATCH
        );
        assert!(agent.hub.queue.has_pending());
        assert!(agent.db.pending_app_mutations(&app, 10).unwrap().is_empty());
        assert_eq!(
            process_events(&agent, BACKGROUND_EVENT_BATCH, false).unwrap(),
            1
        );
        assert_eq!(agent.db.pending_app_mutations(&app, 10).unwrap().len(), 1);
        assert_eq!(agent.hub.queue.dropped(), 0);
        assert_eq!(agent.observer.loss_generation(), 0);
        assert!(!agent.hub.queue.has_pending());
        drop(agent);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn full_flush_consumes_more_than_a_background_limit() {
        let (root, agent, app, pid, save) = fixture("collector-full-flush");
        agent
            .db
            .insert_session(&AppSession::new(
                app.clone(),
                pid,
                SessionSource::ExecutableDiscovery,
            ))
            .unwrap();
        padding(&agent, BACKGROUND_EVENT_BATCH * 2);
        agent
            .hub
            .inject_fs(fs_event(FsEventKind::Write, pid, &save));
        assert_eq!(
            agent.process_events().unwrap(),
            BACKGROUND_EVENT_BATCH * 2 + 1
        );
        assert_eq!(agent.db.pending_app_mutations(&app, 10).unwrap().len(), 1);
        assert_eq!(agent.hub.queue.dropped(), 0);
        assert!(!agent.hub.queue.has_pending());
        drop(agent);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn explicit_procfs_recovery_first_flushes_the_full_observer_watermark() {
        let (root, agent, app, pid, save) = fixture("collector-explicit-recovery");
        agent
            .db
            .insert_session(&AppSession::new(
                app.clone(),
                pid,
                SessionSource::ExecutableDiscovery,
            ))
            .unwrap();
        padding(&agent, BACKGROUND_EVENT_BATCH + 1);
        agent
            .hub
            .inject_fs(fs_event(FsEventKind::Write, pid, &save));
        let (processed, _) = agent.reconcile_live_processes_with_event_count().unwrap();
        assert_eq!(processed, BACKGROUND_EVENT_BATCH + 2);
        assert!(!agent.hub.queue.has_queued_events());
        assert_eq!(agent.db.pending_app_mutations(&app, 10).unwrap().len(), 1);
        assert_eq!(agent.hub.queue.dropped(), 0);
        drop(agent);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_exit_boundary_prevents_foreign_pid_reuse_with_unavailable_procfs_proof() {
        let (root, agent, original, pid, save) = fixture("collector-pid-reuse");
        agent
            .hub
            .inject_fs(fs_event(FsEventKind::Write, pid, &save));
        assert_eq!(process_events(&agent, 1, false).unwrap(), 1);
        let mut exit = process_exec(pid, 1, "/opt/original");
        exit.kind = ProcessEventKind::Exit;
        agent.hub.inject_process(exit);
        agent
            .hub
            .inject_process(process_exec(pid, 1, "/opt/foreign-app"));
        assert_eq!(process_events(&agent, 1, false).unwrap(), 1);
        assert_eq!(process_events(&agent, 1, false).unwrap(), 1);
        let foreign = agent.db.session_for_pid(pid).unwrap().unwrap().app_id;
        assert_ne!(original, foreign);
        assert!(agent.db.associations_for_app(&foreign).unwrap().is_empty());
        assert!(agent
            .db
            .list_dirty_apps()
            .unwrap()
            .iter()
            .any(|dirty| { dirty.app_id == original && dirty.requires_reconciliation }));
        assert_eq!(agent.hub.queue.dropped(), 0);
        drop(agent);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_lost_exit_boundary_cannot_reassign_pending_facts_to_a_reused_pid() {
        let (root, agent, original, pid, save) = fixture("collector-lost-pid-boundary");
        agent
            .hub
            .inject_fs(fs_event(FsEventKind::Write, pid, &save));
        assert_eq!(process_events(&agent, 1, false).unwrap(), 1);
        // The originating process exited and its Exit fact was lost. Without
        // procfs identity evidence, a later Exec must not claim the old write.
        agent.hub.queue.report_loss(1);
        agent
            .hub
            .inject_process(process_exec(pid, 1, "/opt/foreign-app"));
        assert_eq!(process_events(&agent, 1, false).unwrap(), 1);
        // An unstamped Exec cannot restore PID authority after the gap.
        assert!(agent.db.session_for_pid(pid).unwrap().is_none());
        assert!(agent
            .db
            .pending_app_mutations(&original, 10)
            .unwrap()
            .is_empty());
        assert!(agent
            .db
            .list_dirty_apps()
            .unwrap()
            .iter()
            .any(|dirty| { dirty.app_id == original && dirty.requires_reconciliation }));
        assert!(agent.hub.queue.loss_snapshot().is_none());
        assert!(agent.observer.loss_generation() > 0);
        drop(agent);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn loss_remains_sticky_and_procfs_cannot_overtake_a_bounded_fifo_suffix() {
        let (root, agent, original, pid, save) = fixture("collector-loss-suffix");
        padding(&agent, BACKGROUND_EVENT_BATCH);
        agent
            .hub
            .inject_fs(fs_event(FsEventKind::Write, pid, &save));
        agent
            .hub
            .inject_process(process_exec(pid, 1, "/opt/foreign-app"));
        agent.hub.queue.report_loss(1);
        assert_eq!(
            process_events(&agent, BACKGROUND_EVENT_BATCH, false).unwrap(),
            BACKGROUND_EVENT_BATCH
        );
        assert!(agent.hub.queue.loss_snapshot().is_some());
        assert!(agent.hub.queue.has_queued_events());
        assert_eq!(agent.observer.loss_generation(), 1);
        // A periodic recovery is deferred, not a hidden full queue flush.
        Work::RecoverProcesses.run(&agent).unwrap();
        assert!(agent.hub.queue.has_queued_events());
        assert!(agent.db.session_for_pid(pid).unwrap().is_none());
        assert_eq!(
            process_events(&agent, BACKGROUND_EVENT_BATCH, false).unwrap(),
            2
        );
        assert!(agent.db.session_for_pid(pid).unwrap().is_none());
        assert!(agent
            .db
            .pending_app_mutations(&original, 10)
            .unwrap()
            .is_empty());
        assert!(agent.hub.queue.loss_snapshot().is_none());
        assert_eq!(agent.hub.queue.dropped(), 1);
        assert_eq!(
            agent.observer.loss_generation(),
            1,
            "normal bounded suffix handling is not an additional loss"
        );
        // Loss acknowledgement is not a certificate for a guessed identity.
        agent
            .hub
            .inject_fs(fs_event(FsEventKind::Write, pid, &save));
        assert_eq!(agent.process_events().unwrap(), 1);
        assert!(agent
            .db
            .pending_app_mutations(&original, 10)
            .unwrap()
            .is_empty());
        drop(agent);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unwinding_an_engine_turn_preserves_its_pending_proof_state() {
        let (root, agent, _, pid, save) = fixture("collector-unwind");
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = agent.collector.lock().with_engine(
                agent.attribution_engine(),
                |engine| -> Result<()> {
                    engine.ingest_fs(&fs_event(FsEventKind::Write, pid, &save))?;
                    panic!("injected collector panic");
                },
            );
        }))
        .is_err());
        agent
            .collector
            .lock()
            .with_engine(agent.attribution_engine(), |engine| {
                assert_eq!(engine.unresolved_len(), 1);
                Ok(())
            })
            .unwrap();
        drop(agent);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_blocked_collector_turn_does_not_block_the_tokio_io_worker() {
        let (root, agent, _, _, _) = fixture("collector-io-fairness");
        let agent = Arc::new(agent);
        let gate = agent.collector.lock();
        let worker = Arc::clone(&agent);
        let job =
            tokio::spawn(async move { join_work(&worker, |agent| Work::Events.run(agent)).await });
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(
            !job.is_finished(),
            "collector must wait for the shared gate"
        );
        drop(gate);
        tokio::time::timeout(Duration::from_secs(5), job)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(agent.hub.queue.dropped(), 0);
        drop(agent);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn joined_background_panics_rearm_loss_instead_of_disappearing() {
        let (root, agent, _, _, _) = fixture("collector-joined-panic");
        let agent = Arc::new(agent);
        let error = join_work(&agent, |_| panic!("injected blocking collector panic"))
            .await
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("collector blocking worker failed"));
        assert!(agent.hub.queue.loss_snapshot().is_some());
        assert!(agent.observer.loss_generation() > 0);
        assert!(agent.hub.queue.has_pending());
        drop(agent);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn failed_turn_pacing_survives_pending_facts_and_producer_wakeups_then_retries() {
        let (root, agent, app, pid, save) = fixture("collector-failed-turn-pacing");
        agent
            .db
            .insert_session(&AppSession::new(
                app,
                pid,
                SessionSource::ExecutableDiscovery,
            ))
            .unwrap();
        let registry = agent.config.home.join(".local/share/noland-runtime/apps");
        std::fs::create_dir_all(&registry).unwrap();
        let malformed = registry.join("malformed.json");
        std::fs::write(&malformed, b"{invalid").unwrap();
        agent
            .hub
            .inject_fs(fs_event(FsEventKind::Write, pid, &save));
        agent.hub.report_filesystem_gap(pid);
        let agent = Arc::new(agent);
        for attempt in 0..2 {
            assert!(join_work(&agent, |agent| Work::Events.run(agent))
                .await
                .is_err());
            let dropped = agent.hub.queue.dropped();
            assert!(agent.hub.queue.has_queued_events());
            assert!(agent.hub.queue.loss_snapshot().is_some());
            let started = std::time::Instant::now();
            let pacing = pace_after_turn(true);
            tokio::pin!(pacing);
            agent.hub.report_filesystem_gap(pid);
            assert!(
                tokio::time::timeout(Duration::from_millis(25), pacing.as_mut())
                    .await
                    .is_err(),
                "queued work and Notify permits must not bypass failed-turn pacing"
            );
            agent.hub.report_filesystem_gap(pid);
            assert!(agent.hub.queue.loss_snapshot().is_some());
            assert_eq!(agent.hub.queue.dropped(), dropped);
            if attempt == 1 {
                std::fs::remove_file(&malformed).unwrap();
            }
            pacing.await;
            assert!(started.elapsed() >= FAILED_TURN_BACKOFF);
        }
        let dropped = agent.hub.queue.dropped();
        join_work(&agent, |agent| Work::Events.run(agent))
            .await
            .unwrap();
        assert!(!agent.hub.queue.has_queued_events());
        assert!(agent.hub.queue.loss_snapshot().is_none());
        assert_eq!(agent.hub.queue.dropped(), dropped);
        tokio::time::timeout(Duration::from_millis(10), pace_after_turn(false))
            .await
            .expect("successful prefixes must not inherit failure backoff");
        drop(agent);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn loaded_joined_collector_drains_all_prefixes_without_loss_or_io_starvation() {
        let (root, agent, app, pid, save) = fixture("collector-loaded");
        agent
            .db
            .insert_session(&AppSession::new(
                app.clone(),
                pid,
                SessionSource::ExecutableDiscovery,
            ))
            .unwrap();
        let count = BACKGROUND_EVENT_BATCH * 2 + 17;
        for index in 0..count {
            agent.hub.inject_fs(fs_event(
                FsEventKind::Write,
                pid,
                save.with_file_name(format!("save-{index}.dat")),
            ));
        }
        let agent = Arc::new(agent);
        let worker = Arc::clone(&agent);
        let turns = async move {
            let mut turns = 0;
            while worker.hub.queue.has_pending() {
                join_work(&worker, |agent| Work::Events.run(agent))
                    .await
                    .unwrap();
                turns += 1;
            }
            turns
        };
        tokio::pin!(turns);
        let mut timer = tokio::time::interval(Duration::from_millis(1));
        let mut io_ticks = 0;
        let completed_turns = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                tokio::select! {
                    turns = &mut turns => break turns,
                    _ = timer.tick() => io_ticks += 1,
                }
            }
        })
        .await
        .expect("loaded collector must finish without another producer push");
        assert_eq!(completed_turns, 3);
        assert!(
            io_ticks > 1,
            "Tokio timers must run during blocking ingestion"
        );
        assert_eq!(
            agent
                .db
                .pending_app_mutations(&app, count + 1)
                .unwrap()
                .len(),
            count
        );
        assert_eq!(agent.hub.queue.dropped(), 0);
        assert_eq!(agent.observer.loss_generation(), 0);
        assert!(!agent.hub.queue.has_pending());
        drop(agent);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn failed_fresh_preflight_refuses_capture_and_retains_its_failed_fact() {
        let (root, agent, app, pid, _) = fixture("collector-preflight-failure");
        agent
            .db
            .insert_session(&AppSession::new(
                app.clone(),
                pid,
                SessionSource::ExecutableDiscovery,
            ))
            .unwrap();
        let registry = agent.config.home.join(".local/share/noland-runtime/apps");
        std::fs::create_dir_all(&registry).unwrap();
        std::fs::write(registry.join("malformed.json"), b"{invalid").unwrap();
        agent.hub.report_filesystem_gap(pid);
        let storage = noland_storage::LocalStorage::new(root.join("remote"));
        let operation = Uuid::new_v4();
        let before = queued_backup(&agent, operation, &app);
        assert!(crate::backup::run_backup(
            &agent,
            &app,
            BackupMode::CompleteApplication,
            BackupPerformanceMode::Fast,
            &storage,
            &noland_crypto::MasterKey::generate(),
            Some(operation),
        )
        .await
        .is_err());
        assert!(agent.hub.queue.loss_snapshot().is_some());
        assert!(matches!(
            agent.hub.queue.drain().as_slice(),
            [noland_observer::QueuedEvent::FilesystemGap { pid: retained }] if *retained == pid
        ));
        assert!(agent.db.latest_commit(&app).unwrap().is_none());
        assert!(agent
            .db
            .list_dirty_apps()
            .unwrap()
            .iter()
            .any(|dirty| dirty.app_id == app && dirty.requires_reconciliation));
        let record = agent.db.get_operation(operation).unwrap().unwrap();
        let stored: OperationMetrics =
            serde_json::from_value(record.detail_json["metrics"].clone()).unwrap();
        let runtime = agent.db.get_operation_metrics(operation).unwrap().unwrap();
        assert!(stored.discovery_duration_ms >= before.discovery_duration_ms);
        assert_eq!(stored.discovery_duration_ms, runtime.discovery_duration_ms);
        drop(agent);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn fresh_single_backup_discovers_then_flushes_full_watermark_and_retains_timing() {
        let (root, agent, _, pid, _) = fixture("collector-fresh-discovery");
        let app = AppId::desktop("discovery-watermark");
        let install = agent.config.home.join("Applications/Discovery Watermark");
        let executable = install.join("bin/discovered-game");
        let save = install.join("custom-data/save.dat");
        let entries = agent.config.home.join(".local/share/applications");
        std::fs::create_dir_all(&entries).unwrap();
        for (path, bytes) in [
            (&executable, b"fixture executable".as_slice()),
            (&save, b"discovered save".as_slice()),
        ] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        std::fs::write(
            entries.join("discovery-watermark.desktop"),
            format!(
                "[Desktop Entry]\nName=Discovery Watermark\nExec=\"{}\"\nType=Application\n",
                executable.display()
            ),
        )
        .unwrap();
        assert!(agent.db.get_app(&app).unwrap().is_none());
        padding(&agent, BACKGROUND_EVENT_BATCH);
        agent.hub.inject_process(process_exec(pid, 1, &executable));
        agent
            .hub
            .inject_fs(fs_event(FsEventKind::Write, pid, &save));
        let operation = Uuid::new_v4();
        let before = queued_backup(&agent, operation, &app);
        let storage = LocalStorage::new(root.join("remote"));
        let manifest = crate::backup::run_backup(
            &agent,
            &app,
            BackupMode::CompleteApplication,
            BackupPerformanceMode::Fast,
            &storage,
            &noland_crypto::MasterKey::generate(),
            Some(operation),
        )
        .await
        .unwrap();
        assert_eq!(manifest.app.app_id, app);
        assert_eq!(agent.db.session_for_pid(pid).unwrap().unwrap().app_id, app);
        let captured = manifest
            .files
            .iter()
            .find(|file| file.source_path_hint.as_deref() == save.to_str())
            .unwrap();
        assert_eq!(
            captured.file_hash,
            noland_cas::blake3_hex(b"discovered save")
        );
        assert!(!agent.hub.queue.has_pending());
        assert_eq!(agent.hub.queue.dropped(), 0);
        let record = agent.db.get_operation(operation).unwrap().unwrap();
        let stored: OperationMetrics =
            serde_json::from_value(record.detail_json["metrics"].clone()).unwrap();
        let runtime = agent.db.get_operation_metrics(operation).unwrap().unwrap();
        assert!(stored.discovery_duration_ms > before.discovery_duration_ms);
        assert_eq!(stored.discovery_duration_ms, runtime.discovery_duration_ms);
        assert!(stored.total_duration_ms >= stored.discovery_duration_ms);
        drop(agent);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn fresh_backup_flushes_a_mutation_behind_the_batch_limit_before_fast_reuse() {
        let (root, agent, app, pid, save) = fixture("collector-fresh-backup");
        let storage = noland_storage::LocalStorage::new(root.join("remote"));
        let key = noland_crypto::MasterKey::generate();
        let first = crate::backup::run_backup(
            &agent,
            &app,
            BackupMode::CompleteApplication,
            BackupPerformanceMode::Fast,
            &storage,
            &key,
            Some(Uuid::new_v4()),
        )
        .await
        .unwrap();
        assert_eq!(first.files.len(), 1);
        let modified = std::fs::metadata(&save).unwrap().modified().unwrap();
        std::fs::write(&save, b"new!").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&save)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        agent
            .db
            .insert_session(&AppSession::new(
                app.clone(),
                pid,
                SessionSource::ExecutableDiscovery,
            ))
            .unwrap();
        padding(&agent, BACKGROUND_EVENT_BATCH);
        agent
            .hub
            .inject_fs(fs_event(FsEventKind::Write, pid, &save));
        let next = crate::backup::run_backup(
            &agent,
            &app,
            BackupMode::CompleteApplication,
            BackupPerformanceMode::Fast,
            &storage,
            &key,
            Some(Uuid::new_v4()),
        )
        .await
        .unwrap();
        assert_eq!(next.files.len(), 1);
        assert_ne!(first.files[0].file_hash, next.files[0].file_hash);
        assert_eq!(next.files[0].file_hash, noland_cas::blake3_hex(b"new!"));
        assert!(!agent.hub.queue.has_pending());
        assert_eq!(agent.hub.queue.dropped(), 0);
        drop(agent);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn coherent_frozen_retry_precedes_a_failing_later_live_observer_flush() {
        let (root, agent, app, pid, save) = fixture("collector-frozen-retry");
        let storage = InterruptedPublication {
            inner: LocalStorage::new(root.join("remote")),
            fail_manifest: true.into(),
        };
        let key = noland_crypto::MasterKey::generate();
        let operation = Uuid::new_v4();
        let first = crate::backup::run_backup(
            &agent,
            &app,
            BackupMode::CompleteApplication,
            BackupPerformanceMode::Fast,
            &storage,
            &key,
            Some(operation),
        )
        .await
        .unwrap_err();
        assert!(first
            .to_string()
            .contains("interruption after coherent capture"));
        let captured_discovery = agent
            .db
            .get_operation_metrics(operation)
            .unwrap()
            .unwrap()
            .discovery_duration_ms;
        std::fs::write(&save, b"new local save must survive frozen replay").unwrap();
        agent
            .db
            .insert_session(&AppSession::new(
                app.clone(),
                pid,
                SessionSource::ExecutableDiscovery,
            ))
            .unwrap();
        let registry = agent.config.home.join(".local/share/noland-runtime/apps");
        std::fs::create_dir_all(&registry).unwrap();
        std::fs::write(registry.join("malformed.json"), b"{invalid").unwrap();
        agent.hub.report_filesystem_gap(pid);
        // A live discovery/flush prelude would fail here. Authenticated prepared
        // revision selection must precede that work, including in RPC dispatch.
        assert!(agent
            .discover()
            .and_then(|_| agent.process_events().map(|_| ()))
            .is_err());
        assert!(agent.hub.queue.loss_snapshot().is_some());
        let live_loss_generation = agent.observer.loss_generation();
        let live_dropped = agent.hub.queue.dropped();
        let resumed = crate::backup::run_backup(
            &agent,
            &app,
            BackupMode::CompleteApplication,
            BackupPerformanceMode::Fast,
            &storage,
            &key,
            Some(operation),
        )
        .await
        .unwrap();
        assert_eq!(resumed.files[0].file_hash, noland_cas::blake3_hex(b"old!"));
        assert_eq!(
            std::fs::read(&save).unwrap(),
            b"new local save must survive frozen replay"
        );
        assert!(agent.hub.queue.loss_snapshot().is_some());
        assert_eq!(agent.hub.queue.dropped(), live_dropped);
        assert!(matches!(
            agent.hub.queue.drain().as_slice(),
            [noland_observer::QueuedEvent::FilesystemGap { pid: retained }] if *retained == pid
        ));
        assert_eq!(agent.observer.loss_generation(), live_loss_generation);
        assert_eq!(
            agent
                .db
                .get_operation_metrics(operation)
                .unwrap()
                .unwrap()
                .discovery_duration_ms,
            captured_discovery
        );
        drop(agent);
        std::fs::remove_dir_all(root).unwrap();
    }
}
