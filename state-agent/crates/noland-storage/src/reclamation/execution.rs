use super::*;
use crate::{RollingUploadWindow, StorageOperationMetrics};
use std::{future::Future, time::Instant};

/// Unforgeable outside this crate: the frozen plan and repository-wide preflight
/// must be validated before the optimized provider path can remove an exact key.
pub struct PrevalidatedDeletion {
    key: RemoteKey,
}

impl PrevalidatedDeletion {
    pub fn key(&self) -> &RemoteKey {
        &self.key
    }
    #[cfg(test)]
    pub(crate) fn fixture(key: RemoteKey) -> Self {
        Self { key }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeletionProgress {
    pub phase: String,
    pub validated_packs: u64,
    pub completed_objects: u64,
    pub total_objects: u64,
    pub completed_packs: u64,
    pub total_packs: u64,
    pub verified_absent_bytes: u64,
    pub total_bytes: u64,
    pub last_success_at: Option<chrono::DateTime<chrono::Utc>>,
    pub elapsed_ms: u64,
    pub concurrency: usize,
    pub storage_metrics: StorageOperationMetrics,
}

pub trait DeletionObserver: Send + Sync {
    fn progress(&self, progress: &DeletionProgress) -> Result<()>;
}

struct Reporter<'a> {
    progress: DeletionProgress,
    observer: Option<&'a dyn DeletionObserver>,
    provider: &'a dyn SharedStorageProvider,
    baseline: StorageOperationMetrics,
    started: Instant,
}

impl Reporter<'_> {
    fn emit(&mut self, successful_request: bool) -> Result<()> {
        if successful_request {
            self.progress.last_success_at = Some(chrono::Utc::now());
        }
        self.progress.elapsed_ms =
            self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        self.progress.storage_metrics = self
            .provider
            .operation_metrics()
            .saturating_sub(self.baseline);
        self.observer
            .map_or(Ok(()), |observer| observer.progress(&self.progress))
    }
    fn phase(&mut self, phase: &str) -> Result<()> {
        self.progress.phase = phase.into();
        self.emit(false)
    }
}

/// A borrowed rolling window, not detached tasks or fixed waves. On failure stop
/// admission and drain every admitted request before releasing repository access.
/// Cancellation drops/reaps its child futures while the durable pending plan stays.
async fn bounded<I, T, F, Fut, Done>(
    items: I,
    concurrency: usize,
    work: F,
    mut done: Done,
) -> Result<()>
where
    I: IntoIterator,
    I::IntoIter: Send,
    I::Item: Send,
    F: Fn(I::Item) -> Fut + Sync,
    Fut: Future<Output = Result<T>> + Send,
    Done: FnMut(T) -> Result<()> + Send,
{
    let mut items = items.into_iter();
    let mut window = RollingUploadWindow::new(concurrency.clamp(1, 4), 1);
    let mut exhausted = false;
    let mut error = None;
    loop {
        while error.is_none() && !exhausted && window.can_push(0) {
            match items.next() {
                Some(item) => window.push(0, work(item))?,
                None => exhausted = true,
            }
        }
        let Some(result) = window.next().await else {
            break;
        };
        match result.and_then(&mut done) {
            Ok(()) => {}
            Err(failure) if error.is_none() => error = Some(failure),
            Err(_) => {}
        }
    }
    error.map_or(Ok(()), Err)
}

async fn absent(provider: &dyn SharedStorageProvider, key: &RemoteKey) -> Result<()> {
    if provider.stat_for_deletion(key).await?.is_some() {
        return Err(StateError::Storage(
            "full deletion is incomplete; retry the same confirmed plan".into(),
        ));
    }
    Ok(())
}

async fn delete_phase(
    provider: &dyn SharedStorageProvider,
    objects: Vec<(RemoteKey, u64, bool)>,
    reporter: &mut Reporter<'_>,
) -> Result<()> {
    bounded(
        objects,
        reporter.progress.concurrency,
        |(key, size, pack)| async move {
            crate::deletion::validate_deletable_key(&key)?;
            provider
                .delete_prevalidated(&PrevalidatedDeletion { key: key.clone() })
                .await?;
            // A successful DELETE is not evidence of absence. Only count objects
            // after this exact-key check; retain an independent final verification.
            absent(provider, &key).await?;
            Ok((size, pack))
        },
        |(size, pack)| {
            reporter.progress.completed_objects += 1;
            reporter.progress.completed_packs += u64::from(pack);
            reporter.progress.verified_absent_bytes =
                reporter.progress.verified_absent_bytes.saturating_add(size);
            reporter.emit(true)
        },
    )
    .await
}

pub async fn execute_full_deletion_observed(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    plan: &FullDeletionPlan,
    observer: Option<&dyn DeletionObserver>,
) -> Result<()> {
    plan.validate()?;
    let concurrency = provider.deletion_concurrency().clamp(1, 4);
    let mut reporter = Reporter {
        progress: DeletionProgress {
            phase: "validating".into(),
            validated_packs: 0,
            completed_objects: 0,
            total_objects: (plan.metadata.len() + plan.packs.len()) as u64,
            completed_packs: 0,
            total_packs: plan.packs.len() as u64,
            verified_absent_bytes: 0,
            total_bytes: plan.physical_bytes(),
            last_success_at: None,
            elapsed_ms: 0,
            concurrency,
            storage_metrics: StorageOperationMetrics::default(),
        },
        observer,
        provider,
        baseline: provider.operation_metrics(),
        started: Instant::now(),
    };
    reporter.emit(false)?;
    let activities = crate::repository_activities(provider, master).await?;
    if activities.len() != 1 || activities[0].operation_id != plan.operation_id {
        return Err(StateError::Conflict(
            "full deletion requires exclusive repository admission".into(),
        ));
    }
    let existing = load_full_deletion(provider, master, plan.plan_id).await?;
    if let Some(existing) = &existing {
        if existing != plan {
            return Err(StateError::Conflict(
                "confirmed deletion plan changed".into(),
            ));
        }
        if completed(provider, master, plan.plan_id).await? {
            provider.delete_object(&pending_key(plan.plan_id)).await?;
            return Ok(());
        }
    }
    let current = inventory(provider, master).await?;
    provider.check_full_deletion_support().await?;
    if existing.is_none() && crate::compact_json_hash(&current)? != plan.inventory_hash {
        return Err(StateError::Conflict(
            "cloud backups changed since confirmation; preview again".into(),
        ));
    }
    let selected: BTreeSet<_> = plan.targets.iter().map(|t| t.deletion.bundle_id).collect();
    let packs: BTreeSet<_> = plan.packs.keys().cloned().collect();
    for bundle in current
        .iter()
        .filter(|b| selected.contains(&b.target.deletion.bundle_id))
    {
        if plan.target_hashes.get(&bundle.target.deletion.bundle_id)
            != Some(&(bundle.manifest_hash.clone(), bundle.index_hash.clone()))
        {
            return Err(StateError::Conflict(
                "a confirmed bundle was replaced; deletion stopped".into(),
            ));
        }
    }
    let preflight = plan
        .packs
        .iter()
        .map(|(id, size)| (RemoteKey::new(pack_key(id)), *size))
        .collect::<Vec<_>>();
    bounded(
        preflight,
        concurrency,
        |(key, expected)| {
            let resumed = existing.is_some();
            async move {
                match provider.stat_for_deletion(&key).await? {
                    Some(meta) if meta.key == key && meta.size == expected => Ok(()),
                    None if resumed => Ok(()),
                    _ => Err(StateError::Conflict(
                        "a confirmed pack changed or disappeared; preview again".into(),
                    )),
                }
            }
        },
        |()| {
            reporter.progress.validated_packs += 1;
            reporter.emit(true)
        },
    )
    .await?;
    if current
        .iter()
        .any(|b| !selected.contains(&b.target.deletion.bundle_id) && !b.packs.is_disjoint(&packs))
    {
        return Err(StateError::Conflict("an unconfirmed backup now references these packs; deletion stopped without widening consent".into()));
    }
    // Every preflight request is drained before any durable destructive state.
    reporter.phase("preparing")?;
    if existing.is_none() {
        provider
            .put_small_versioned(
                Bytes::from(
                    seal_metadata(&derive_keys(master).catalog, b"full-deletion-plan-v1", plan)?
                        .envelope,
                ),
                &plan_key(plan.plan_id),
            )
            .await?;
        if load_full_deletion(provider, master, plan.plan_id)
            .await?
            .as_ref()
            != Some(plan)
        {
            return Err(StateError::Integrity("deletion plan not durable".into()));
        }
    }
    provider
        .put_small_versioned(
            Bytes::from(
                seal_metadata(
                    &derive_keys(master).catalog,
                    b"pending-deletion-v1",
                    &plan.plan_id,
                )?
                .envelope,
            ),
            &pending_key(plan.plan_id),
        )
        .await?;
    let mut retired_ids = retired_pack_ids(provider, master).await?;
    retired_ids.extend(plan.packs.keys().cloned());
    if retired_ids.len() > LIMIT_PACKS {
        return Err(StateError::Storage(
            "pack retirement index exceeds bounds".into(),
        ));
    }
    let retired = RetiredPacks {
        plan_id: plan.plan_id,
        packs: retired_ids.clone(),
    };
    provider
        .put_small_versioned(
            Bytes::from(
                seal_metadata(&derive_keys(master).catalog, b"retired-packs-v1", &retired)?
                    .envelope,
            ),
            &retired_key("INDEX"),
        )
        .await?;
    if retired_pack_ids(provider, master).await? != retired_ids {
        return Err(StateError::Integrity(
            "pack retirement index was not durably published".into(),
        ));
    }
    for target in &plan.targets {
        crate::deletion::persist_bundle_deletion_receipt(
            provider,
            master,
            &target.deletion,
            plan.plan_id,
        )
        .await?;
        reporter.emit(true)?;
    }
    // Publish all tombstones, then remove only the frozen commit markers before
    // any other metadata/packs. Each metadata object is deleted exactly once.
    reporter.phase("unpublishing")?;
    let is_marker = |key: &str| key.ends_with(&format!("/{COMMITTED_MARKER}"));
    delete_phase(
        provider,
        plan.metadata
            .iter()
            .filter(|(key, _)| is_marker(key))
            .map(|(key, size)| (RemoteKey::new(key), *size, false))
            .collect(),
        &mut reporter,
    )
    .await?;
    reporter.phase("deleting_metadata")?;
    delete_phase(
        provider,
        plan.metadata
            .iter()
            .filter(|(key, _)| !is_marker(key))
            .map(|(key, size)| (RemoteKey::new(key), *size, false))
            .collect(),
        &mut reporter,
    )
    .await?;
    reporter.phase("deleting_packs")?;
    delete_phase(
        provider,
        plan.packs
            .iter()
            .map(|(id, size)| (RemoteKey::new(pack_key(id)), *size, true))
            .collect(),
        &mut reporter,
    )
    .await?;
    reporter.phase("verifying")?;
    let verification = plan
        .metadata
        .keys()
        .cloned()
        .chain(plan.packs.keys().map(|id| pack_key(id)))
        .collect::<Vec<_>>();
    bounded(
        verification,
        concurrency,
        |key| async move { absent(provider, &RemoteKey::new(key)).await },
        |()| reporter.emit(true),
    )
    .await?;
    reporter.phase("finalizing")?;
    for target in &plan.targets {
        provider
            .remove_empty_bundle(&target.deletion.app_id, target.deletion.bundle_id)
            .await?;
    }
    provider
        .put_small_versioned(
            Bytes::from(
                seal_metadata(
                    &derive_keys(master).catalog,
                    b"full-deletion-done-v1",
                    &plan.plan_id,
                )?
                .envelope,
            ),
            &done_key(plan.plan_id),
        )
        .await?;
    if !completed(provider, master, plan.plan_id).await? {
        return Err(StateError::Integrity(
            "deletion completion receipt not durable".into(),
        ));
    }
    provider.delete_object(&pending_key(plan.plan_id)).await?;
    reporter.emit(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[tokio::test]
    async fn four_worker_window_refills_before_a_straggler_completes() {
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let refill = tokio::sync::Notify::new();
        let mut completed = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(5),
            bounded(
                0..12,
                4,
                |id| {
                    let active = &active;
                    let peak = &peak;
                    let refill = &refill;
                    async move {
                        let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(now, Ordering::SeqCst);
                        if id == 0 {
                            refill.notified().await;
                        } else {
                            tokio::task::yield_now().await;
                        }
                        if id == 4 {
                            refill.notify_one();
                        }
                        active.fetch_sub(1, Ordering::SeqCst);
                        Ok(id)
                    }
                },
                |id| {
                    completed.push(id);
                    Ok(())
                },
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(peak.load(Ordering::SeqCst), 4);
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert_eq!(completed.len(), 12);
        assert!(
            completed.iter().position(|id| *id == 4) < completed.iter().position(|id| *id == 0)
        );
    }

    #[tokio::test]
    async fn a_failed_request_stops_admission_and_drains_the_admitted_window() {
        let active = AtomicUsize::new(0);
        let started = AtomicUsize::new(0);
        let drained = AtomicUsize::new(0);
        let result = bounded(
            0..100,
            4,
            |id| {
                let active = &active;
                let started = &started;
                let drained = &drained;
                async move {
                    started.fetch_add(1, Ordering::SeqCst);
                    active.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(if id == 0 { 1 } else { 20 })).await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    drained.fetch_add(1, Ordering::SeqCst);
                    if id == 0 {
                        Err(StateError::Storage("fixture failure".into()))
                    } else {
                        Ok(())
                    }
                }
            },
            |()| Ok(()),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(started.load(Ordering::SeqCst), 4);
        assert_eq!(drained.load(Ordering::SeqCst), 4);
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }
}
