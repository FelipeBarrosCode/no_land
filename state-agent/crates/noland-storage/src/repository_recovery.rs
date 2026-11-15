//! Explicit, externally authorized recovery, never age-based claim expiry.
//!
//! The caller must verify the ENTIRE provider inventory (not only registered or
//! running PCs), check the remaining agent, and obtain human confirmation that
//! no external clients are writing. A fresh execution-unique activity fences
//! ordinary repository admission while exactly one authenticated claim clears.
//! Other authenticated non-recovery claims may be frozen into that exact plan;
//! retaining them never grants permission to clear or change their objects.
use async_trait::async_trait;
use chrono::Utc;
use noland_state_core::{Result, StateError};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use uuid::Uuid;

use crate::RepositoryActivity;

pub const RECOVERY_OWNER_PREFIX: &str = "lock-recovery:";
const RETAINED_SNAPSHOT_DOMAIN: &str = "noland-repository-lock-recovery-retained-snapshot-v1";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RecoveryAuthorization {
    pub instance_id: u64,
    pub all_owned_instance_ids: Vec<u64>,
    pub active_operations: u64,
    pub local_admission_unlocked: bool,
    pub checked_at: i64,
    pub no_other_writers_confirmed: bool,
}

impl RecoveryAuthorization {
    pub fn validate(&self, now: i64) -> Result<()> {
        if !self.no_other_writers_confirmed
            || self.instance_id == 0
            || self.all_owned_instance_ids != [self.instance_id]
            || self.active_operations != 0
            || !self.local_admission_unlocked
            || self.checked_at > now
            || now.saturating_sub(self.checked_at) > 60
        {
            return Err(StateError::Conflict(
                "lock recovery requires fresh complete provider inventory, an idle sole PC, and explicit confirmation that all other writers have stopped".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryPhase {
    Prepared,
    Complete,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetainedActivitySnapshot {
    pub count: usize,
    /// Domain-separated BLAKE3 of semantic activities, sorted by claim ID.
    pub hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RecoveryReceipt {
    pub recovery_id: Uuid,
    pub target: RepositoryActivity,
    /// Constant-size commitment to frozen authenticated claims which must stay
    /// unchanged. It never grants authorization to remove any retained claim.
    /// Missing legacy commitment means there must be no other claims.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_snapshot: Option<RetainedActivitySnapshot>,
    pub authorization: RecoveryAuthorization,
    pub phase: RecoveryPhase,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecoveryCheckpoint {
    pub prepared: RecoveryReceipt,
    pub complete: Option<RecoveryReceipt>,
}

pub fn validate_resume_snapshot(
    checkpoint: &RecoveryCheckpoint,
    authorization: &RecoveryAuthorization,
    original_execution_stopped: bool,
    activities: Vec<RepositoryActivity>,
) -> Result<()> {
    authorization.validate(Utc::now().timestamp())?;
    let prepared = &checkpoint.prepared;
    validate_recovery_selection(prepared.recovery_id, &prepared.target, &[])?;
    if !original_execution_stopped
        || prepared.phase != RecoveryPhase::Prepared
        || prepared.authorization.instance_id != authorization.instance_id
        || prepared.recovery_id == prepared.target.claim_id
        || prepared.target.owner.starts_with(RECOVERY_OWNER_PREFIX)
    {
        return Err(StateError::Conflict(
            "original recovery owner, plan or stopped-execution proof could not be verified".into(),
        ));
    }
    let fence = RepositoryActivity {
        claim_id: prepared.recovery_id,
        operation_id: prepared.recovery_id,
        owner: format!("{RECOVERY_OWNER_PREFIX}{}", prepared.recovery_id),
        access: Default::default(),
        instance_id: None,
    };
    let target = if let Some(complete) = &checkpoint.complete {
        if complete.phase != RecoveryPhase::Complete
            || complete.recovery_id != prepared.recovery_id
            || complete.target != prepared.target
            || complete.retained_snapshot != prepared.retained_snapshot
            || complete.authorization.instance_id != authorization.instance_id
        {
            return Err(StateError::Integrity(
                "recovery completion does not match its original prepared plan".into(),
            ));
        }
        None
    } else {
        Some(&prepared.target)
    };
    require_frozen_activities(activities, prepared, target, Some(&fence))
}

/// Resume ONLY the original authenticated plan, after its own execution is
/// verified stopped and fresh human approval/provider-settlement confirmation.
/// Never invent another claim ID, recreate a prepared plan, or repeat a target
/// deletion already covered by an authenticated completion receipt.
pub async fn resume_repository_activity(
    store: &dyn RecoveryMetadataStore,
    checkpoint: RecoveryCheckpoint,
    authorization: RecoveryAuthorization,
    original_execution_stopped: bool,
) -> Result<RecoveryReceipt> {
    require_no_pending_deletion(store).await?;
    validate_resume_snapshot(
        &checkpoint,
        &authorization,
        original_execution_stopped,
        store.activities().await?,
    )?;
    let prepared = checkpoint.prepared;
    let fence = RepositoryActivity {
        claim_id: prepared.recovery_id,
        operation_id: prepared.recovery_id,
        owner: format!("{RECOVERY_OWNER_PREFIX}{}", prepared.recovery_id),
        access: Default::default(),
        instance_id: None,
    };
    let complete = match checkpoint.complete {
        Some(complete) => complete,
        None => {
            store.remove_activity(&prepared.target).await?;
            require_frozen_activities(store.activities().await?, &prepared, None, Some(&fence))?;
            require_no_pending_deletion(store).await?;
            let complete = RecoveryReceipt {
                recovery_id: prepared.recovery_id,
                target: prepared.target.clone(),
                retained_snapshot: prepared.retained_snapshot.clone(),
                authorization,
                phase: RecoveryPhase::Complete,
            };
            store.write_receipt(&complete).await?;
            complete
        }
    };
    require_no_pending_deletion(store).await?;
    require_frozen_activities(store.activities().await?, &prepared, None, Some(&fence))?;
    store.remove_activity(&fence).await?;
    if store.activities().await?.iter().any(|activity| {
        activity.claim_id == prepared.target.claim_id || activity.claim_id == fence.claim_id
    }) {
        return Err(StateError::Conflict(
            "resumed recovery absence could not be confirmed".into(),
        ));
    }
    Ok(complete)
}

/// Narrow metadata-only backend. Implementations authenticate each activity,
/// reject ambiguous/partial listings, and compare the exact activity again
/// immediately before deleting its execution-unique object. No pack/bundle API.
#[async_trait]
pub trait RecoveryMetadataStore: Send + Sync {
    async fn activities(&self) -> Result<Vec<RepositoryActivity>>;
    async fn has_pending_deletion(&self) -> Result<bool>;
    async fn create_activity(&self, activity: &RepositoryActivity) -> Result<()>;
    async fn remove_activity(&self, expected: &RepositoryActivity) -> Result<()>;
    async fn write_receipt(&self, receipt: &RecoveryReceipt) -> Result<()>;
}

fn validate_recovery_selection(
    recovery_id: Uuid,
    target: &RepositoryActivity,
    retained_activities: &[RepositoryActivity],
) -> Result<()> {
    target.validate()?;
    if recovery_id.is_nil() {
        return Err(StateError::Integrity(
            "invalid repository recovery identity".into(),
        ));
    }
    if target.owner.starts_with(RECOVERY_OWNER_PREFIX) || recovery_id == target.claim_id {
        return Err(StateError::Conflict(
            "an interrupted lock-recovery attempt needs its original owner's verified recovery; it cannot be force-cleared as a PC claim".into(),
        ));
    }
    let mut ids = HashSet::from([target.claim_id, recovery_id]);
    for activity in retained_activities {
        activity.validate()?;
        if activity.owner.starts_with(RECOVERY_OWNER_PREFIX) || !ids.insert(activity.claim_id) {
            return Err(StateError::Conflict(
                "retained activities must be distinct non-recovery claims, excluding the target and recovery fence".into(),
            ));
        }
    }
    Ok(())
}

fn snapshot_commitment(
    activities: &[RepositoryActivity],
) -> Result<Option<RetainedActivitySnapshot>> {
    if activities.is_empty() {
        return Ok(None);
    }
    let mut sorted = activities.iter().collect::<Vec<_>>();
    sorted.sort_by_key(|activity| activity.claim_id);
    Ok(Some(RetainedActivitySnapshot {
        count: sorted.len(),
        hash: crate::compact_json_hash(&(RETAINED_SNAPSHOT_DOMAIN, sorted))?,
    }))
}

fn require_frozen_activities(
    observed: Vec<RepositoryActivity>,
    receipt: &RecoveryReceipt,
    target: Option<&RepositoryActivity>,
    fence: Option<&RepositoryActivity>,
) -> Result<()> {
    let (selected, retained): (Vec<_>, Vec<_>) = observed.into_iter().partition(|activity| {
        activity.claim_id == receipt.target.claim_id || activity.claim_id == receipt.recovery_id
    });
    require_exact_activities(selected, target.into_iter().chain(fence).cloned().collect())?;
    validate_recovery_selection(receipt.recovery_id, &receipt.target, &retained)?;
    if snapshot_commitment(&retained)? != receipt.retained_snapshot {
        return Err(StateError::Conflict(
            "retained repository activities changed; inspect again before recovering a lock".into(),
        ));
    }
    Ok(())
}

fn require_exact_activities(
    mut observed: Vec<RepositoryActivity>,
    mut expected: Vec<RepositoryActivity>,
) -> Result<()> {
    observed.sort_by_key(|a| a.claim_id);
    expected.sort_by_key(|a| a.claim_id);
    if observed != expected {
        return Err(StateError::Conflict(
            "repository activity changed; inspect again before recovering a lock".into(),
        ));
    }
    Ok(())
}

async fn require_no_pending_deletion(store: &dyn RecoveryMetadataStore) -> Result<()> {
    if store.has_pending_deletion().await? {
        return Err(StateError::Conflict(
            "unfinished deletion metadata exists; resolve that exact deletion instead of clearing a lock".into(),
        ));
    }
    Ok(())
}

/// One attempt only. The caller consumes its preview before calling, retains
/// action exclusion, and must not retry on a lost response. After target delete
/// starts, ANY failure/cancellation leaves the recovery fence and prepared
/// receipt intact: a reaped process does not prove a provider request settled.
pub async fn recover_repository_activity(
    store: &dyn RecoveryMetadataStore,
    recovery_id: Uuid,
    target: RepositoryActivity,
    authorization: RecoveryAuthorization,
) -> Result<RecoveryReceipt> {
    recover_repository_activity_preserving(store, recovery_id, target, Vec::new(), authorization)
        .await
}

/// Recover one exact claim while preserving every other claim frozen by the
/// caller's authenticated preview. The entire snapshot must match before and
/// after admission; unrelated claims or recovery fences never get ignored.
/// The same stopped-writers proof and one-attempt-only rules apply as above.
pub async fn recover_repository_activity_preserving(
    store: &dyn RecoveryMetadataStore,
    recovery_id: Uuid,
    target: RepositoryActivity,
    retained_activities: Vec<RepositoryActivity>,
    authorization: RecoveryAuthorization,
) -> Result<RecoveryReceipt> {
    recover_repository_activity_preserving_with_clock(
        store,
        recovery_id,
        target,
        retained_activities,
        authorization,
        || Utc::now().timestamp(),
    )
    .await
}

async fn recover_repository_activity_preserving_with_clock(
    store: &dyn RecoveryMetadataStore,
    recovery_id: Uuid,
    target: RepositoryActivity,
    retained_activities: Vec<RepositoryActivity>,
    authorization: RecoveryAuthorization,
    now: impl Fn() -> i64 + Send + Sync,
) -> Result<RecoveryReceipt> {
    authorization.validate(now())?;
    validate_recovery_selection(recovery_id, &target, &retained_activities)?;
    require_no_pending_deletion(store).await?;
    let mut receipt = RecoveryReceipt {
        recovery_id,
        target: target.clone(),
        retained_snapshot: snapshot_commitment(&retained_activities)?,
        authorization,
        phase: RecoveryPhase::Prepared,
    };
    require_frozen_activities(store.activities().await?, &receipt, Some(&target), None)?;
    // A complete authenticated inventory can outlast the human approval's
    // freshness window. Never treat its start time as fresh mutation authority.
    receipt.authorization.validate(now())?;
    store.write_receipt(&receipt).await?;
    receipt.authorization.validate(now())?;
    let fence = RepositoryActivity {
        claim_id: recovery_id,
        operation_id: recovery_id,
        owner: format!("{RECOVERY_OWNER_PREFIX}{recovery_id}"),
        access: Default::default(),
        instance_id: None,
    };
    // Do not clean up after an uncertain create: it may still settle remotely.
    store.create_activity(&fence).await?;
    let admitted = async {
        require_frozen_activities(
            store.activities().await?,
            &receipt,
            Some(&target),
            Some(&fence),
        )?;
        require_no_pending_deletion(store).await?;
        receipt.authorization.validate(now())
    }
    .await;
    if let Err(error) = admitted {
        // No target mutation has started. Only our acknowledged, unique fence
        // may be removed; never clean up a competing execution's activity.
        store.remove_activity(&fence).await?;
        return Err(error);
    }
    // The exact fence now holds and target mutation begins. From here on,
    // finish the durable receipt/fence protocol without new expiry failures.
    store.remove_activity(&target).await?;
    require_frozen_activities(store.activities().await?, &receipt, None, Some(&fence))?;
    require_no_pending_deletion(store).await?;
    receipt.phase = RecoveryPhase::Complete;
    store.write_receipt(&receipt).await?;
    // Completion publication is also a provider round trip: do not drop the
    // fence if a retained claim or pending-deletion state changed meanwhile.
    require_no_pending_deletion(store).await?;
    require_frozen_activities(store.activities().await?, &receipt, None, Some(&fence))?;
    store.remove_activity(&fence).await?;
    // A new execution may now enter. Verify our identities are absent, not that
    // the whole repository remains forever idle.
    if store
        .activities()
        .await?
        .iter()
        .any(|a| a.claim_id == target.claim_id || a.claim_id == fence.claim_id)
    {
        return Err(StateError::Conflict(
            "lock removal could not be confirmed; do not retry automatically".into(),
        ));
    }
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicI64, Ordering},
        Arc, Mutex,
    };

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Boundary {
        InventoryRead,
        PreparedWritten,
        FenceCreated,
        TargetRemoved,
        CompleteWritten,
    }

    #[derive(Default)]
    struct Data {
        activities: Vec<RepositoryActivity>,
        receipts: Vec<RecoveryReceipt>,
        removed: Vec<Uuid>,
        pending: bool,
        compete: bool,
        uncertain_delete: bool,
        corrupt_snapshot: bool,
        change_before_delete: bool,
        fail_complete_receipt: bool,
        retained_id: Option<Uuid>,
        change_retained_at: Option<Boundary>,
        pending_at: Option<Boundary>,
        clock: Option<Arc<AtomicI64>>,
        expire_at: Option<Boundary>,
    }
    struct Store(Arc<Mutex<Data>>);

    fn apply_boundary(data: &mut Data, boundary: Boundary) {
        if data.expire_at == Some(boundary) {
            data.clock.as_ref().unwrap().fetch_add(61, Ordering::SeqCst);
            data.expire_at = None;
        }
        if data.change_retained_at == Some(boundary) {
            let retained_id = data.retained_id;
            let retained = data
                .activities
                .iter_mut()
                .find(|activity| Some(activity.claim_id) == retained_id)
                .unwrap();
            retained.operation_id = Uuid::new_v4();
            data.change_retained_at = None;
        }
        if data.pending_at == Some(boundary) {
            data.pending = true;
            data.pending_at = None;
        }
    }

    #[async_trait]
    impl RecoveryMetadataStore for Store {
        async fn activities(&self) -> Result<Vec<RepositoryActivity>> {
            let mut data = self.0.lock().unwrap();
            if data.corrupt_snapshot {
                return Err(StateError::Integrity("unauthenticated metadata".into()));
            }
            apply_boundary(&mut data, Boundary::InventoryRead);
            Ok(data.activities.clone())
        }
        async fn has_pending_deletion(&self) -> Result<bool> {
            Ok(self.0.lock().unwrap().pending)
        }
        async fn create_activity(&self, a: &RepositoryActivity) -> Result<()> {
            let mut data = self.0.lock().unwrap();
            data.activities.push(a.clone());
            if data.compete {
                data.activities.push(activity());
            }
            apply_boundary(&mut data, Boundary::FenceCreated);
            Ok(())
        }
        async fn remove_activity(&self, expected: &RepositoryActivity) -> Result<()> {
            let mut data = self.0.lock().unwrap();
            if !expected.owner.starts_with(RECOVERY_OWNER_PREFIX) {
                if data.change_before_delete {
                    data.activities[0].operation_id = Uuid::new_v4();
                }
                if data.uncertain_delete {
                    return Err(StateError::Storage("lost provider response".into()));
                }
            }
            let index = data
                .activities
                .iter()
                .position(|a| a == expected)
                .ok_or_else(|| {
                    StateError::Conflict("activity changed immediately before delete".into())
                })?;
            data.activities.remove(index);
            data.removed.push(expected.claim_id);
            if !expected.owner.starts_with(RECOVERY_OWNER_PREFIX) {
                apply_boundary(&mut data, Boundary::TargetRemoved);
            }
            Ok(())
        }
        async fn write_receipt(&self, receipt: &RecoveryReceipt) -> Result<()> {
            let mut data = self.0.lock().unwrap();
            if data.fail_complete_receipt && receipt.phase == RecoveryPhase::Complete {
                return Err(StateError::Storage("receipt publication failed".into()));
            }
            data.receipts.push(receipt.clone());
            if receipt.phase == RecoveryPhase::Complete {
                apply_boundary(&mut data, Boundary::CompleteWritten);
            } else {
                apply_boundary(&mut data, Boundary::PreparedWritten);
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn resumed_uncertain_delete_or_failed_completion_keeps_original_fence() {
        for uncertain in [true, false] {
            let (checkpoint, store) = interrupted_fixture(false);
            let recovery_id = checkpoint.prepared.recovery_id;
            {
                let mut data = store.0.lock().unwrap();
                data.uncertain_delete = uncertain;
                data.fail_complete_receipt = !uncertain;
            }
            assert!(
                resume_repository_activity(&store, checkpoint, authorization(), true)
                    .await
                    .is_err()
            );
            let data = store.0.lock().unwrap();
            assert!(data.activities.iter().any(|a| a.claim_id == recovery_id));
            assert!(!data.removed.contains(&recovery_id));
            assert_eq!(data.receipts.len(), 1);
        }
    }

    fn activity() -> RepositoryActivity {
        RepositoryActivity {
            claim_id: Uuid::new_v4(),
            operation_id: Uuid::new_v4(),
            owner: "abandoned-owner".into(),
            access: Default::default(),
            instance_id: None,
        }
    }
    fn authorization() -> RecoveryAuthorization {
        RecoveryAuthorization {
            instance_id: 42,
            all_owned_instance_ids: vec![42],
            active_operations: 0,
            local_admission_unlocked: true,
            checked_at: Utc::now().timestamp(),
            no_other_writers_confirmed: true,
        }
    }
    fn fixture() -> (RepositoryActivity, Store) {
        let target = activity();
        let store = Store(Arc::new(Mutex::new(Data {
            activities: vec![target.clone()],
            ..Data::default()
        })));
        (target, store)
    }

    fn reader_activity() -> RepositoryActivity {
        let instance_id = Uuid::new_v4();
        RepositoryActivity {
            owner: format!("abandoned-reader-{instance_id}"),
            access: crate::RepositoryAccess::RestoreRead,
            instance_id: Some(instance_id),
            ..activity()
        }
    }

    fn invalidate_activity(activity: &mut RepositoryActivity, mutation: usize) {
        match mutation {
            0 => activity.claim_id = Uuid::nil(),
            1 => activity.operation_id = Uuid::nil(),
            2 => activity.owner.clear(),
            3 => activity.owner = " \n".into(),
            4 => activity.instance_id = Some(Uuid::nil()),
            5 => activity.instance_id = None,
            _ => activity.owner = format!("{RECOVERY_OWNER_PREFIX}malformed-reader"),
        }
    }

    fn retained_fixture(count: usize) -> (RepositoryActivity, Vec<RepositoryActivity>, Store) {
        let target = reader_activity();
        let retained = (0..count).map(|_| reader_activity()).collect::<Vec<_>>();
        let mut activities = retained.clone();
        activities.push(target.clone());
        let store = Store(Arc::new(Mutex::new(Data {
            activities,
            retained_id: retained.first().map(|activity| activity.claim_id),
            ..Data::default()
        })));
        (target, retained, store)
    }

    fn retained_interrupted_fixture(
        complete: bool,
    ) -> (RecoveryCheckpoint, Vec<RepositoryActivity>, Store) {
        let (mut checkpoint, store) = interrupted_fixture(complete);
        let retained = vec![reader_activity(), reader_activity()];
        checkpoint.prepared.retained_snapshot = snapshot_commitment(&retained).unwrap();
        if let Some(completion) = &mut checkpoint.complete {
            completion.retained_snapshot = checkpoint.prepared.retained_snapshot.clone();
        }
        let mut data = store.0.lock().unwrap();
        data.activities.extend(retained.clone());
        data.retained_id = Some(retained[0].claim_id);
        data.receipts[0] = checkpoint.prepared.clone();
        if let Some(completion) = &checkpoint.complete {
            data.receipts[1] = completion.clone();
        }
        drop(data);
        (checkpoint, retained, store)
    }

    fn interrupted_fixture(complete: bool) -> (RecoveryCheckpoint, Store) {
        let (target, store) = fixture();
        let prepared = RecoveryReceipt {
            recovery_id: Uuid::new_v4(),
            target: target.clone(),
            retained_snapshot: None,
            authorization: authorization(),
            phase: RecoveryPhase::Prepared,
        };
        let completion = complete.then(|| RecoveryReceipt {
            phase: RecoveryPhase::Complete,
            ..prepared.clone()
        });
        let mut data = store.0.lock().unwrap();
        data.receipts.push(prepared.clone());
        if let Some(completion) = &completion {
            data.activities.clear();
            data.receipts.push(completion.clone());
        }
        data.activities.push(RepositoryActivity {
            claim_id: prepared.recovery_id,
            operation_id: prepared.recovery_id,
            owner: format!("{RECOVERY_OWNER_PREFIX}{}", prepared.recovery_id),
            access: Default::default(),
            instance_id: None,
        });
        drop(data);
        (
            RecoveryCheckpoint {
                prepared,
                complete: completion,
            },
            store,
        )
    }

    #[tokio::test]
    async fn verified_owner_resumes_the_exact_original_plan_without_a_new_claim_or_prepared_record()
    {
        let (checkpoint, store) = interrupted_fixture(false);
        let original = checkpoint.prepared.clone();
        assert!(
            resume_repository_activity(&store, checkpoint.clone(), authorization(), false)
                .await
                .is_err()
        );
        assert!(store.0.lock().unwrap().removed.is_empty());
        let result = resume_repository_activity(&store, checkpoint, authorization(), true)
            .await
            .unwrap();
        let data = store.0.lock().unwrap();
        assert_eq!(result.recovery_id, original.recovery_id);
        assert_eq!(
            data.removed,
            [original.target.claim_id, original.recovery_id]
        );
        assert_eq!(data.receipts.len(), 2);
        assert_eq!(data.receipts[0], original);
        assert!(data.activities.is_empty());
    }

    #[tokio::test]
    async fn authenticated_completion_resumes_only_fence_release_never_the_target_delete() {
        let (checkpoint, store) = interrupted_fixture(true);
        let id = checkpoint.prepared.recovery_id;
        resume_repository_activity(&store, checkpoint, authorization(), true)
            .await
            .unwrap();
        let data = store.0.lock().unwrap();
        assert_eq!(data.removed, [id]);
        assert_eq!(data.receipts.len(), 2);
        assert!(data.activities.is_empty());
    }

    #[tokio::test]
    async fn targeted_recovery_preserves_one_or_two_interrupted_readers_exactly() {
        for count in [1, 2] {
            let (target, retained, store) = retained_fixture(count);
            let id = Uuid::new_v4();
            let receipt = recover_repository_activity_preserving(
                &store,
                id,
                target.clone(),
                retained.clone(),
                authorization(),
            )
            .await
            .unwrap();
            let data = store.0.lock().unwrap();
            assert_eq!(data.activities, retained);
            assert_eq!(data.removed, [target.claim_id, id]);
            assert_eq!(data.receipts.len(), 2);
            assert_eq!(
                receipt.retained_snapshot,
                snapshot_commitment(&retained).unwrap()
            );
            assert_eq!(receipt.retained_snapshot.as_ref().unwrap().count, count);
            assert_eq!(
                data.receipts[0].retained_snapshot,
                data.receipts[1].retained_snapshot
            );
            assert_eq!(data.receipts[1], receipt);
        }
    }

    #[tokio::test]
    async fn old_single_claim_api_never_implicitly_retains_unselected_claims() {
        let (target, retained, store) = retained_fixture(1);
        assert!(recover_repository_activity(
            &store,
            Uuid::new_v4(),
            target.clone(),
            authorization()
        )
        .await
        .is_err());
        let data = store.0.lock().unwrap();
        assert!(data.removed.is_empty());
        assert!(data.receipts.is_empty());
        assert!(data.activities.contains(&target));
        assert!(data.activities.contains(&retained[0]));
    }

    #[tokio::test]
    async fn unexpected_changed_duplicate_or_recovery_retained_claims_fail_before_mutation() {
        for mutation in 0..9 {
            let (target, mut retained, store) = retained_fixture(2);
            let recovery_id = Uuid::new_v4();
            {
                let mut data = store.0.lock().unwrap();
                match mutation {
                    0 => data.activities.push(reader_activity()),
                    1 => data.activities[0].operation_id = Uuid::new_v4(),
                    2 => {
                        data.activities.remove(0);
                    }
                    3 => retained.push(retained[0].clone()),
                    4 => retained.push(target.clone()),
                    5 => retained[0].claim_id = recovery_id,
                    6 | 7 => {
                        retained[0].owner = format!("{RECOVERY_OWNER_PREFIX}other");
                        if mutation == 7 {
                            retained[0].access = crate::RepositoryAccess::Exclusive;
                            retained[0].instance_id = None;
                        }
                        data.activities[0] = retained[0].clone();
                    }
                    _ => {
                        let duplicate = data.activities[0].clone();
                        data.activities.push(duplicate);
                    }
                }
            }
            assert!(
                recover_repository_activity_preserving(
                    &store,
                    recovery_id,
                    target.clone(),
                    retained,
                    authorization(),
                )
                .await
                .is_err(),
                "mutation {mutation}"
            );
            let data = store.0.lock().unwrap();
            assert!(data.removed.is_empty(), "mutation {mutation}");
            assert!(data.receipts.is_empty(), "mutation {mutation}");
            assert!(data.activities.contains(&target), "mutation {mutation}");
        }
    }

    #[tokio::test]
    async fn changed_retained_or_raced_pending_state_keeps_the_fence_after_target_removal() {
        for boundary in [
            Boundary::FenceCreated,
            Boundary::TargetRemoved,
            Boundary::CompleteWritten,
        ] {
            for pending in [false, true] {
                let (target, retained, store) = retained_fixture(2);
                let recovery_id = Uuid::new_v4();
                {
                    let mut data = store.0.lock().unwrap();
                    if pending {
                        data.pending_at = Some(boundary);
                    } else {
                        data.change_retained_at = Some(boundary);
                    }
                }
                assert!(recover_repository_activity_preserving(
                    &store,
                    recovery_id,
                    target.clone(),
                    retained.clone(),
                    authorization(),
                )
                .await
                .is_err());
                let data = store.0.lock().unwrap();
                assert!(retained
                    .iter()
                    .all(|activity| !data.removed.contains(&activity.claim_id)));
                if boundary == Boundary::FenceCreated {
                    assert_eq!(data.removed, [recovery_id]);
                    assert!(data.activities.contains(&target));
                } else {
                    assert_eq!(data.removed, [target.claim_id]);
                    assert!(data
                        .activities
                        .iter()
                        .any(|activity| activity.claim_id == recovery_id));
                }
                assert_eq!(
                    data.receipts.len(),
                    if boundary == Boundary::CompleteWritten {
                        2
                    } else {
                        1
                    }
                );
            }
        }
    }

    #[tokio::test]
    async fn targeted_recovery_still_requires_every_authorization_and_no_pending_deletion() {
        for mutation in 0..9 {
            let (target, retained, store) = retained_fixture(2);
            let mut proof = authorization();
            match mutation {
                0 => proof.no_other_writers_confirmed = false,
                1 => proof.all_owned_instance_ids.clear(),
                2 => proof.all_owned_instance_ids.push(43),
                3 => proof.active_operations = 1,
                4 => proof.checked_at -= 61,
                5 => proof.checked_at += 60,
                6 => proof.local_admission_unlocked = false,
                7 => proof.instance_id = 0,
                _ => store.0.lock().unwrap().pending = true,
            }
            assert!(
                recover_repository_activity_preserving(
                    &store,
                    Uuid::new_v4(),
                    target,
                    retained,
                    proof,
                )
                .await
                .is_err(),
                "mutation {mutation}"
            );
            let data = store.0.lock().unwrap();
            assert!(data.removed.is_empty());
            assert!(data.receipts.is_empty());
        }
    }

    #[tokio::test]
    async fn invalid_target_retained_or_recovery_identity_fails_before_any_mutation() {
        for invalid_target in [false, true] {
            for mutation in 0..8 {
                let (mut target, mut retained, store) = retained_fixture(2);
                let recovery_id = if mutation == 7 {
                    Uuid::nil()
                } else {
                    if invalid_target {
                        invalidate_activity(&mut target, mutation);
                    } else {
                        invalidate_activity(&mut retained[0], mutation);
                    }
                    Uuid::new_v4()
                };
                {
                    let mut data = store.0.lock().unwrap();
                    data.activities = retained.clone();
                    data.activities.push(target.clone());
                }
                let original = store.0.lock().unwrap().activities.clone();
                assert!(recover_repository_activity_preserving(
                    &store,
                    recovery_id,
                    target,
                    retained,
                    authorization(),
                )
                .await
                .is_err());
                let data = store.0.lock().unwrap();
                assert!(data.removed.is_empty());
                assert!(data.receipts.is_empty());
                assert_eq!(data.activities, original);
            }
        }
    }

    #[tokio::test]
    async fn malformed_checkpoint_identities_cannot_resume_even_when_retained_hash_matches() {
        for complete in [false, true] {
            for invalid_target in [false, true] {
                for mutation in 0..7 {
                    let (mut checkpoint, mut retained, store) =
                        retained_interrupted_fixture(complete);
                    {
                        let mut data = store.0.lock().unwrap();
                        if invalid_target {
                            let original = checkpoint.prepared.target.claim_id;
                            checkpoint.prepared.target.access =
                                crate::RepositoryAccess::RestoreRead;
                            checkpoint.prepared.target.instance_id = Some(Uuid::new_v4());
                            invalidate_activity(&mut checkpoint.prepared.target, mutation);
                            if let Some(activity) = data
                                .activities
                                .iter_mut()
                                .find(|activity| activity.claim_id == original)
                            {
                                *activity = checkpoint.prepared.target.clone();
                            }
                            if let Some(completion) = &mut checkpoint.complete {
                                completion.target = checkpoint.prepared.target.clone();
                            }
                        } else {
                            let original = retained[0].claim_id;
                            invalidate_activity(&mut retained[0], mutation);
                            *data
                                .activities
                                .iter_mut()
                                .find(|activity| activity.claim_id == original)
                                .unwrap() = retained[0].clone();
                            checkpoint.prepared.retained_snapshot =
                                snapshot_commitment(&retained).unwrap();
                            if let Some(completion) = &mut checkpoint.complete {
                                completion.retained_snapshot =
                                    checkpoint.prepared.retained_snapshot.clone();
                            }
                        }
                    }
                    assert!(
                        resume_repository_activity(&store, checkpoint, authorization(), true,)
                            .await
                            .is_err()
                    );
                    assert!(store.0.lock().unwrap().removed.is_empty());
                }
            }
        }
        let (mut checkpoint, _, store) = retained_interrupted_fixture(true);
        checkpoint.prepared.recovery_id = Uuid::nil();
        checkpoint.complete.as_mut().unwrap().recovery_id = Uuid::nil();
        assert!(
            resume_repository_activity(&store, checkpoint, authorization(), true)
                .await
                .is_err()
        );
        assert!(store.0.lock().unwrap().removed.is_empty());
    }

    #[tokio::test]
    async fn authorization_expiry_blocks_each_pre_target_boundary_but_not_durable_completion() {
        for boundary in [
            Boundary::InventoryRead,
            Boundary::PreparedWritten,
            Boundary::FenceCreated,
            Boundary::TargetRemoved,
            Boundary::CompleteWritten,
        ] {
            let (target, retained, store) = retained_fixture(2);
            let proof = authorization();
            let checked_at = proof.checked_at;
            let clock = Arc::new(AtomicI64::new(checked_at));
            let recovery_id = Uuid::new_v4();
            {
                let mut data = store.0.lock().unwrap();
                data.clock = Some(clock.clone());
                data.expire_at = Some(boundary);
            }
            let result = recover_repository_activity_preserving_with_clock(
                &store,
                recovery_id,
                target.clone(),
                retained.clone(),
                proof,
                || clock.load(Ordering::SeqCst),
            )
            .await;
            assert_eq!(clock.load(Ordering::SeqCst), checked_at + 61);
            let data = store.0.lock().unwrap();
            assert!(data
                .receipts
                .iter()
                .all(|receipt| receipt.authorization.checked_at == checked_at));
            assert!(retained
                .iter()
                .all(|activity| data.activities.contains(activity)));
            match boundary {
                Boundary::InventoryRead | Boundary::PreparedWritten => {
                    assert!(result.is_err());
                    assert!(data.removed.is_empty());
                    assert!(data.activities.contains(&target));
                    assert!(!data
                        .activities
                        .iter()
                        .any(|activity| activity.claim_id == recovery_id));
                    assert_eq!(
                        data.receipts.len(),
                        usize::from(boundary == Boundary::PreparedWritten)
                    );
                }
                Boundary::FenceCreated => {
                    assert!(result.is_err());
                    assert_eq!(data.removed, [recovery_id]);
                    assert!(data.activities.contains(&target));
                    assert_eq!(data.receipts.len(), 1);
                }
                Boundary::TargetRemoved | Boundary::CompleteWritten => {
                    assert!(result.is_ok());
                    assert_eq!(data.removed, [target.claim_id, recovery_id]);
                    assert_eq!(data.activities, retained);
                    assert_eq!(data.receipts.len(), 2);
                }
            }
        }
    }

    #[tokio::test]
    async fn resume_preserves_frozen_claims_and_completion_only_releases_original_fence() {
        for complete in [false, true] {
            let (checkpoint, retained, store) = retained_interrupted_fixture(complete);
            let original = checkpoint.prepared.clone();
            assert!(
                resume_repository_activity(&store, checkpoint.clone(), authorization(), false,)
                    .await
                    .is_err()
            );
            let receipt = resume_repository_activity(&store, checkpoint, authorization(), true)
                .await
                .unwrap();
            let data = store.0.lock().unwrap();
            assert_eq!(data.activities, retained);
            assert_eq!(data.receipts.len(), 2);
            assert_eq!(data.receipts[0], original);
            assert_eq!(receipt.retained_snapshot, original.retained_snapshot);
            assert_eq!(
                data.removed,
                if complete {
                    vec![original.recovery_id]
                } else {
                    vec![original.target.claim_id, original.recovery_id]
                }
            );
        }
    }

    #[tokio::test]
    async fn changed_or_missing_retained_claim_or_checkpoint_commitment_blocks_resume() {
        for complete in [false, true] {
            for mutation in 0..8 {
                let (mut checkpoint, _, store) = retained_interrupted_fixture(complete);
                match mutation {
                    0 => {
                        let mut data = store.0.lock().unwrap();
                        let id = data.retained_id.unwrap();
                        data.activities
                            .iter_mut()
                            .find(|activity| activity.claim_id == id)
                            .unwrap()
                            .access = crate::RepositoryAccess::Exclusive;
                    }
                    1 => {
                        let mut data = store.0.lock().unwrap();
                        let id = data.retained_id.unwrap();
                        data.activities.retain(|activity| activity.claim_id != id);
                    }
                    2 => store.0.lock().unwrap().activities.push(reader_activity()),
                    3 => {
                        checkpoint
                            .prepared
                            .retained_snapshot
                            .as_mut()
                            .unwrap()
                            .count += 1;
                    }
                    4 => {
                        checkpoint.prepared.retained_snapshot.as_mut().unwrap().hash =
                            "blake3:incorrect".into();
                    }
                    5 => checkpoint.prepared.retained_snapshot = None,
                    6 => {
                        if let Some(completion) = &mut checkpoint.complete {
                            completion.retained_snapshot = None;
                        } else {
                            store.0.lock().unwrap().pending = true;
                        }
                    }
                    _ => {
                        let mut data = store.0.lock().unwrap();
                        let mut other_fence = reader_activity();
                        other_fence.owner = format!("{RECOVERY_OWNER_PREFIX}another");
                        data.activities.push(other_fence);
                    }
                }
                assert!(
                    resume_repository_activity(&store, checkpoint, authorization(), true)
                        .await
                        .is_err(),
                    "complete {complete}, mutation {mutation}"
                );
                assert!(store.0.lock().unwrap().removed.is_empty());
            }
        }
    }

    #[tokio::test]
    async fn uncertain_targeted_recovery_or_resume_never_deletes_a_retained_claim() {
        for resumed in [false, true] {
            for uncertain_delete in [false, true] {
                let (checkpoint, retained, store) = retained_interrupted_fixture(false);
                if !resumed {
                    // Begin a new targeted attempt, without an existing fence
                    // or prepared receipt, so both uncertain boundaries run.
                    let mut data = store.0.lock().unwrap();
                    data.activities
                        .retain(|activity| activity.claim_id != checkpoint.prepared.recovery_id);
                    data.receipts.clear();
                }
                {
                    let mut data = store.0.lock().unwrap();
                    data.uncertain_delete = uncertain_delete;
                    data.fail_complete_receipt = !uncertain_delete;
                }
                if resumed {
                    assert!(resume_repository_activity(
                        &store,
                        checkpoint.clone(),
                        authorization(),
                        true,
                    )
                    .await
                    .is_err());
                } else {
                    assert!(recover_repository_activity_preserving(
                        &store,
                        checkpoint.prepared.recovery_id,
                        checkpoint.prepared.target.clone(),
                        retained.clone(),
                        authorization(),
                    )
                    .await
                    .is_err());
                }
                let data = store.0.lock().unwrap();
                assert!(retained
                    .iter()
                    .all(|activity| data.activities.contains(activity)
                        && !data.removed.contains(&activity.claim_id)));
                assert!(!data.removed.contains(&checkpoint.prepared.recovery_id));
                assert_eq!(data.receipts.len(), 1);
            }
        }
    }

    #[tokio::test]
    async fn a_new_targeted_attempt_cannot_adopt_or_ignore_an_existing_recovery_fence() {
        let (checkpoint, retained, store) = retained_interrupted_fixture(false);
        assert!(recover_repository_activity_preserving(
            &store,
            Uuid::new_v4(),
            checkpoint.prepared.target.clone(),
            retained.clone(),
            authorization(),
        )
        .await
        .is_err());
        let data = store.0.lock().unwrap();
        assert!(data.removed.is_empty());
        assert_eq!(data.receipts, [checkpoint.prepared]);
        assert!(retained
            .iter()
            .all(|activity| data.activities.contains(activity)));
    }

    #[test]
    fn legacy_receipts_decode_without_a_commitment_and_keep_the_exact_old_json_shape() {
        let old = serde_json::json!({
            "recovery_id": Uuid::new_v4(),
            "target": activity(),
            "authorization": authorization(),
            "phase": "prepared",
        });
        let receipt: RecoveryReceipt = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(receipt.retained_snapshot, None);
        assert_eq!(serde_json::to_value(receipt).unwrap(), old);
        let (checkpoint, store) = interrupted_fixture(true);
        let mut old_checkpoint = serde_json::to_value(&checkpoint).unwrap();
        old_checkpoint["prepared"]
            .as_object_mut()
            .unwrap()
            .remove("retained_snapshot");
        old_checkpoint["complete"]
            .as_object_mut()
            .unwrap()
            .remove("retained_snapshot");
        let decoded: RecoveryCheckpoint = serde_json::from_value(old_checkpoint).unwrap();
        assert_eq!(decoded, checkpoint);
        validate_resume_snapshot(
            &decoded,
            &authorization(),
            true,
            store.0.lock().unwrap().activities.clone(),
        )
        .unwrap();
        let mut extra = store.0.lock().unwrap().activities.clone();
        extra.push(reader_activity());
        assert!(validate_resume_snapshot(&decoded, &authorization(), true, extra).is_err());
    }

    #[test]
    fn retained_snapshot_is_order_independent_domain_separated_and_binds_every_semantic_field() {
        let original = vec![reader_activity(), reader_activity()];
        let expected = snapshot_commitment(&original).unwrap();
        let mut reordered = original.clone();
        reordered.reverse();
        assert_eq!(snapshot_commitment(&reordered).unwrap(), expected);
        let mut sorted = original.iter().collect::<Vec<_>>();
        sorted.sort_by_key(|activity| activity.claim_id);
        assert_ne!(
            expected.as_ref().unwrap().hash,
            crate::compact_json_hash(&sorted).unwrap()
        );
        for mutation in 0..5 {
            let mut changed = original.clone();
            match mutation {
                0 => changed[0].claim_id = Uuid::new_v4(),
                1 => changed[0].operation_id = Uuid::new_v4(),
                2 => changed[0].owner.push_str("-changed"),
                3 => changed[0].access = crate::RepositoryAccess::Exclusive,
                _ => changed[0].instance_id = Some(Uuid::new_v4()),
            }
            assert_ne!(snapshot_commitment(&changed).unwrap(), expected);
        }
        assert_eq!(snapshot_commitment(&[]).unwrap(), None);
    }

    #[tokio::test]
    async fn recovery_receipts_stay_below_16_kib_with_1024_normal_claims() {
        let (target, retained, store) = retained_fixture(1023);
        let receipt = recover_repository_activity_preserving(
            &store,
            Uuid::new_v4(),
            target,
            retained.clone(),
            authorization(),
        )
        .await
        .unwrap();
        let master = noland_crypto::MasterKey::generate();
        let data = store.0.lock().unwrap();
        assert_eq!(data.activities, retained);
        assert_eq!(receipt.retained_snapshot.as_ref().unwrap().count, 1023);
        for receipt in &data.receipts {
            let json = serde_json::to_vec(receipt).unwrap();
            assert!(json.len() < 2048);
            let sealed = crate::seal_metadata(
                &noland_crypto::derive_keys(&master).catalog,
                b"repository-lock-recovery-v1",
                receipt,
            )
            .unwrap();
            assert!(sealed.envelope.len() < 16 * 1024);
            let reopened: RecoveryReceipt = crate::open_metadata_with_limit(
                &noland_crypto::derive_keys(&master).catalog,
                b"repository-lock-recovery-v1",
                sealed.envelope,
                16 * 1024,
            )
            .unwrap();
            assert_eq!(&reopened, receipt);
        }
    }

    #[tokio::test]
    async fn unknown_missing_target_changed_plan_and_competing_activity_block_resume() {
        for mutation in 0..4 {
            let (mut checkpoint, store) = interrupted_fixture(false);
            match mutation {
                0 => {
                    store.0.lock().unwrap().activities.remove(0);
                }
                1 => checkpoint.prepared.authorization.instance_id = 43,
                2 => store.0.lock().unwrap().activities.push(activity()),
                _ => store.0.lock().unwrap().pending = true,
            }
            assert!(
                resume_repository_activity(&store, checkpoint, authorization(), true)
                    .await
                    .is_err()
            );
            assert!(store.0.lock().unwrap().removed.is_empty());
        }
    }

    #[test]
    fn complete_inventory_and_fresh_human_authorization_are_required() {
        let now = Utc::now().timestamp();
        for mutation in 0..8 {
            let mut proof = authorization();
            match mutation {
                0 => proof.no_other_writers_confirmed = false,
                1 => proof.all_owned_instance_ids.clear(),
                2 => proof.all_owned_instance_ids.push(43),
                3 => proof.active_operations = 1,
                4 => proof.checked_at = now - 61,
                5 => proof.checked_at = now + 1,
                6 => proof.local_admission_unlocked = false,
                _ => proof.instance_id = 0,
            }
            assert!(proof.validate(now).is_err());
        }
        authorization().validate(now).unwrap();
    }

    #[tokio::test]
    async fn exact_target_and_only_our_fence_clear_with_prepared_and_complete_receipts() {
        let (target, store) = fixture();
        let id = Uuid::new_v4();
        let receipt = recover_repository_activity(&store, id, target.clone(), authorization())
            .await
            .unwrap();
        let data = store.0.lock().unwrap();
        assert!(data.activities.is_empty());
        assert_eq!(data.removed, [target.claim_id, id]);
        assert_eq!(data.receipts.len(), 2);
        assert_eq!(data.receipts[0].phase, RecoveryPhase::Prepared);
        assert_eq!(receipt.phase, RecoveryPhase::Complete);
        assert_eq!(data.receipts[1], receipt);
    }

    #[tokio::test]
    async fn changed_multiple_corrupt_pending_or_recovery_claims_never_clear_a_target() {
        for mutation in 0..6 {
            let (mut target, store) = fixture();
            {
                let mut data = store.0.lock().unwrap();
                match mutation {
                    0 => data.activities[0].operation_id = Uuid::new_v4(),
                    1 => data.activities.push(activity()),
                    2 => data.corrupt_snapshot = true,
                    3 => data.pending = true,
                    4 => target.owner = format!("{RECOVERY_OWNER_PREFIX}other"),
                    _ => data.change_before_delete = true,
                }
            }
            assert!(recover_repository_activity(
                &store,
                Uuid::new_v4(),
                target.clone(),
                authorization()
            )
            .await
            .is_err());
            let data = store.0.lock().unwrap();
            assert!(!data.removed.contains(&target.claim_id));
            assert!(data
                .activities
                .iter()
                .any(|a| a.claim_id == target.claim_id));
        }
    }

    #[tokio::test]
    async fn competing_execution_is_preserved_and_only_our_pre_delete_fence_cleans_up() {
        let (target, store) = fixture();
        store.0.lock().unwrap().compete = true;
        let id = Uuid::new_v4();
        assert!(
            recover_repository_activity(&store, id, target.clone(), authorization())
                .await
                .is_err()
        );
        let data = store.0.lock().unwrap();
        assert_eq!(data.removed, [id]);
        assert_eq!(data.activities.len(), 2);
        assert!(data.activities.contains(&target));
    }

    #[tokio::test]
    async fn uncertain_target_delete_retains_fence_and_forbids_an_automatic_new_attempt() {
        let (target, store) = fixture();
        store.0.lock().unwrap().uncertain_delete = true;
        let id = Uuid::new_v4();
        assert!(
            recover_repository_activity(&store, id, target.clone(), authorization())
                .await
                .is_err()
        );
        {
            let data = store.0.lock().unwrap();
            assert!(data.removed.is_empty());
            assert_eq!(data.activities.len(), 2);
            assert_eq!(data.receipts.len(), 1);
        }
        assert!(
            recover_repository_activity(&store, Uuid::new_v4(), target, authorization())
                .await
                .is_err()
        );
        assert_eq!(store.0.lock().unwrap().receipts.len(), 1);
    }

    #[tokio::test]
    async fn failed_final_receipt_leaves_a_fence_even_after_target_absence_was_verified() {
        let (target, store) = fixture();
        store.0.lock().unwrap().fail_complete_receipt = true;
        let id = Uuid::new_v4();
        assert!(
            recover_repository_activity(&store, id, target.clone(), authorization())
                .await
                .is_err()
        );
        let data = store.0.lock().unwrap();
        assert_eq!(data.removed, [target.claim_id]);
        assert_eq!(data.activities.len(), 1);
        assert_eq!(data.activities[0].claim_id, id);
        assert_eq!(data.receipts.len(), 1);
    }

    struct GatedStore {
        inner: Store,
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }
    #[async_trait]
    impl RecoveryMetadataStore for GatedStore {
        async fn activities(&self) -> Result<Vec<RepositoryActivity>> {
            self.inner.activities().await
        }
        async fn has_pending_deletion(&self) -> Result<bool> {
            self.inner.has_pending_deletion().await
        }
        async fn create_activity(&self, activity: &RepositoryActivity) -> Result<()> {
            self.inner.create_activity(activity).await
        }
        async fn remove_activity(&self, expected: &RepositoryActivity) -> Result<()> {
            if !expected.owner.starts_with(RECOVERY_OWNER_PREFIX) {
                self.entered.notify_one();
                self.release.notified().await;
            }
            self.inner.remove_activity(expected).await
        }
        async fn write_receipt(&self, receipt: &RecoveryReceipt) -> Result<()> {
            self.inner.write_receipt(receipt).await
        }
    }

    #[tokio::test]
    async fn cancellation_during_target_removal_retains_the_fence_and_prepared_receipt() {
        let (target, inner) = fixture();
        let store = Arc::new(GatedStore {
            inner,
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let execution_store = store.clone();
        let expected = target.clone();
        let task = tokio::spawn(async move {
            recover_repository_activity(
                execution_store.as_ref(),
                Uuid::new_v4(),
                expected,
                authorization(),
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), store.entered.notified())
            .await
            .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let data = store.inner.0.lock().unwrap();
        assert!(data.activities.contains(&target));
        assert_eq!(data.activities.len(), 2);
        assert!(data.removed.is_empty());
        assert_eq!(data.receipts.len(), 1);
        assert_eq!(data.receipts[0].phase, RecoveryPhase::Prepared);
    }

    struct DiskStore {
        provider: crate::LocalStorage,
        master: noland_crypto::MasterKey,
        target_id: Uuid,
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }
    #[async_trait]
    impl RecoveryMetadataStore for DiskStore {
        async fn activities(&self) -> Result<Vec<RepositoryActivity>> {
            crate::repository_activities(&self.provider, &self.master).await
        }
        async fn has_pending_deletion(&self) -> Result<bool> {
            Ok(!crate::pending_deletions(&self.provider, &self.master)
                .await?
                .is_empty())
        }
        async fn create_activity(&self, activity: &RepositoryActivity) -> Result<()> {
            use crate::SharedStorageProvider;
            self.provider
                .put_small_versioned(
                    bytes::Bytes::from(
                        crate::seal_metadata(
                            &noland_crypto::derive_keys(&self.master).catalog,
                            b"repository-activity-v1",
                            activity,
                        )?
                        .envelope,
                    ),
                    &crate::RemoteKey::new(format!(
                        "maintenance/activity/{}.enc",
                        activity.claim_id
                    )),
                )
                .await?;
            Ok(())
        }
        async fn remove_activity(&self, expected: &RepositoryActivity) -> Result<()> {
            if expected.claim_id == self.target_id {
                self.entered.notify_one();
                self.release.notified().await;
            }
            crate::clear_repository_activity(&self.provider, &self.master, expected).await
        }
        async fn write_receipt(&self, receipt: &RecoveryReceipt) -> Result<()> {
            use crate::SharedStorageProvider;
            let phase = match receipt.phase {
                RecoveryPhase::Prepared => "prepared",
                RecoveryPhase::Complete => "complete",
            };
            self.provider
                .put_small_versioned(
                    bytes::Bytes::from(
                        crate::seal_metadata(
                            &noland_crypto::derive_keys(&self.master).catalog,
                            b"repository-lock-recovery-v1",
                            receipt,
                        )?
                        .envelope,
                    ),
                    &crate::RemoteKey::new(format!(
                        "maintenance/lock-recoveries/{}.{phase}.enc",
                        receipt.recovery_id
                    )),
                )
                .await?;
            Ok(())
        }
    }

    #[tokio::test]
    async fn real_repository_admission_cannot_enter_during_recovery_and_backups_are_untouched() {
        use crate::SharedStorageProvider;
        let root = std::env::current_dir()
            .unwrap()
            .join("target/lock-recovery")
            .join(Uuid::new_v4().to_string());
        let target = activity();
        let store = Arc::new(DiskStore {
            provider: crate::LocalStorage::new(&root),
            master: noland_crypto::MasterKey::generate(),
            target_id: target.claim_id,
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        store.create_activity(&target).await.unwrap();
        for name in ["packs/keep.pack", "bundles/keep/manifest.enc"] {
            store
                .provider
                .put_small_versioned(
                    bytes::Bytes::from_static(b"untouched encrypted application data"),
                    &crate::RemoteKey::new(name),
                )
                .await
                .unwrap();
        }
        let execution = store.clone();
        let task = tokio::spawn(async move {
            recover_repository_activity(execution.as_ref(), Uuid::new_v4(), target, authorization())
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), store.entered.notified())
            .await
            .unwrap();
        assert!(crate::RepositoryGuard::acquire(
            &store.provider,
            &store.master,
            Uuid::new_v4(),
            "competing PC",
        )
        .await
        .is_err());
        assert_eq!(store.activities().await.unwrap().len(), 2);
        store.release.notify_one();
        let receipt = task.await.unwrap().unwrap();
        assert_eq!(receipt.phase, RecoveryPhase::Complete);
        let guard = crate::RepositoryGuard::acquire(
            &store.provider,
            &store.master,
            Uuid::new_v4(),
            "next authorized PC",
        )
        .await
        .unwrap();
        guard.release().await.unwrap();
        for name in ["packs/keep.pack", "bundles/keep/manifest.enc"] {
            assert_eq!(
                std::fs::read(root.join(name)).unwrap(),
                b"untouched encrypted application data"
            );
        }
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn real_targeted_recovery_preserves_reader_ciphertext_and_backup_data() {
        use crate::{RepositoryAccess, RepositoryGuard, SharedStorageProvider};
        let root = std::env::temp_dir().join(format!("targeted-recovery-{}", Uuid::new_v4()));
        let target = reader_activity();
        let retained = reader_activity();
        let store = Arc::new(DiskStore {
            provider: crate::LocalStorage::new(&root),
            master: noland_crypto::MasterKey::generate(),
            target_id: target.claim_id,
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        store.create_activity(&target).await.unwrap();
        store.create_activity(&retained).await.unwrap();
        let retained_path = root.join(format!("maintenance/activity/{}.enc", retained.claim_id));
        let original_retained_ciphertext = std::fs::read(&retained_path).unwrap();
        let artifacts = ["packs/keep.pack", "bundles/keep/manifest.enc"];
        for name in artifacts {
            store
                .provider
                .put_small_versioned(
                    bytes::Bytes::from_static(b"untouched encrypted application data"),
                    &crate::RemoteKey::new(name),
                )
                .await
                .unwrap();
        }
        let execution = store.clone();
        let preserved = retained.clone();
        let task = tokio::spawn(async move {
            recover_repository_activity_preserving(
                execution.as_ref(),
                Uuid::new_v4(),
                target,
                vec![preserved],
                authorization(),
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), store.entered.notified())
            .await
            .unwrap();
        assert!(RepositoryGuard::acquire_for_instance(
            &store.provider,
            &store.master,
            Uuid::new_v4(),
            "another reader",
            Uuid::new_v4(),
            RepositoryAccess::RestoreRead,
        )
        .await
        .is_err());
        assert!(
            RepositoryGuard::acquire(&store.provider, &store.master, Uuid::new_v4(), "writer",)
                .await
                .is_err()
        );
        assert_eq!(store.activities().await.unwrap().len(), 3);
        store.release.notify_one();
        let receipt = task.await.unwrap().unwrap();
        assert_eq!(receipt.retained_snapshot.as_ref().unwrap().count, 1);
        assert_eq!(
            store.activities().await.unwrap().as_slice(),
            std::slice::from_ref(&retained)
        );
        assert_eq!(
            std::fs::read(&retained_path).unwrap(),
            original_retained_ciphertext
        );
        // The unselected interrupted reader still fences destructive work.
        assert!(RepositoryGuard::acquire(
            &store.provider,
            &store.master,
            Uuid::new_v4(),
            "deletion",
        )
        .await
        .is_err());
        let read = RepositoryGuard::acquire_for_instance(
            &store.provider,
            &store.master,
            Uuid::new_v4(),
            "next reader",
            Uuid::new_v4(),
            RepositoryAccess::RestoreRead,
        )
        .await
        .unwrap();
        read.release().await.unwrap();
        assert_eq!(store.activities().await.unwrap(), [retained]);
        for name in artifacts {
            assert_eq!(
                std::fs::read(root.join(name)).unwrap(),
                b"untouched encrypted application data"
            );
        }
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }
}
