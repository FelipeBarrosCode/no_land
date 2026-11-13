//! Repository-wide admission for managed transfers and destructive maintenance.
//! Claims never expire on a timer: a stalled client cannot lose protection while
//! its workers are alive. Cancellation/crash leaves a fail-closed claim which
//! only its owner may clear after proving that local execution has stopped.
use crate::{
    commit::download_metadata, open_metadata, seal_metadata, RemoteKey, SharedStorageProvider,
};
use bytes::Bytes;
use noland_crypto::{derive_keys, MasterKey};
use noland_state_core::{Result, StateError};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryActivity {
    /// Unique per execution, not per operation: two PCs can retry the same plan.
    pub claim_id: Uuid,
    pub operation_id: Uuid,
    pub owner: String,
}

fn key(id: Uuid) -> RemoteKey {
    RemoteKey::new(format!("maintenance/activity/{id}.enc"))
}

pub async fn repository_activities(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
) -> Result<Vec<RepositoryActivity>> {
    let mut output = Vec::new();
    for entry in provider
        .list_prefix(&RemoteKey::new("maintenance/activity"))
        .await?
    {
        if entry.is_prefix {
            continue;
        }
        if output.len() >= 1024 {
            return Err(StateError::Conflict(
                "too many repository activities; maintenance required".into(),
            ));
        }
        let bytes = download_metadata(provider, &entry.key, 16 * 1024).await?;
        let activity: RepositoryActivity = open_metadata(
            &derive_keys(master).catalog,
            b"repository-activity-v1",
            bytes,
        )?;
        if key(activity.claim_id) != entry.key {
            return Err(StateError::Integrity(
                "repository activity identity mismatch".into(),
            ));
        }
        output.push(activity);
    }
    Ok(output)
}

pub async fn clear_repository_activity(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    activity: &RepositoryActivity,
) -> Result<()> {
    let k = key(activity.claim_id);
    if provider.stat(&k).await?.is_none() {
        return Ok(());
    }
    let current: RepositoryActivity = open_metadata(
        &derive_keys(master).catalog,
        b"repository-activity-v1",
        download_metadata(provider, &k, 16 * 1024).await?,
    )?;
    if current != *activity {
        return Err(StateError::Conflict(
            "repository activity belongs to another execution".into(),
        ));
    }
    provider.delete_object(&k).await
}

pub struct RepositoryGuard<'a> {
    provider: &'a dyn SharedStorageProvider,
    master: &'a MasterKey,
    activity: RepositoryActivity,
}

impl<'a> RepositoryGuard<'a> {
    pub async fn acquire(
        provider: &'a dyn SharedStorageProvider,
        master: &'a MasterKey,
        operation_id: Uuid,
        owner: &str,
    ) -> Result<Self> {
        for plan in crate::pending_deletions(provider, master).await? {
            if plan.operation_id != operation_id {
                return Err(StateError::Conflict(format!(
                    "repository has unfinished deletion {}; resume that exact deletion first",
                    plan.plan_id
                )));
            }
        }
        let activity = RepositoryActivity {
            claim_id: Uuid::new_v4(),
            operation_id,
            owner: owner.into(),
        };
        let k = key(activity.claim_id);
        // Never check-then-overwrite an operation-named claim. A second retry
        // must see the first execution, not replace its protection in place.
        provider
            .put_small_versioned(
                Bytes::from(
                    seal_metadata(
                        &derive_keys(master).catalog,
                        b"repository-activity-v1",
                        &activity,
                    )?
                    .envelope,
                ),
                &k,
            )
            .await?;
        let admitted = async {
            let activities = repository_activities(provider, master).await?;
            if activities.len() != 1 || activities[0] != activity {
                return Err(StateError::Conflict("another PC has active or interrupted Shared Storage work; finish or resolve it before continuing".into()));
            }
            // A previous execution can finish (or leave pending destructive
            // work) between our first pending read and publication of this
            // claim. Recheck after winning admission, not only before it.
            for plan in crate::pending_deletions(provider, master).await? {
                if plan.operation_id != operation_id {
                    return Err(StateError::Conflict(format!("repository has unfinished deletion {}; resume that exact deletion first",plan.plan_id)));
                }
            }
            Ok(())
        }.await;
        if let Err(error) = admitted {
            let _ = clear_repository_activity(provider, master, &activity).await;
            return Err(error);
        }
        Ok(Self {
            provider,
            master,
            activity,
        })
    }

    pub async fn release(self) -> Result<()> {
        clear_repository_activity(self.provider, self.master, &self.activity).await
    }
}
