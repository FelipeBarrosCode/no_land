//! Repository admission for managed transfers and destructive maintenance.
//! Restore readers on different instances may coexist; all writers and
//! destructive maintenance still require repository-wide exclusive admission.
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
use std::collections::HashSet;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryAccess {
    #[default]
    Exclusive,
    RestoreRead,
}

impl RepositoryAccess {
    fn is_exclusive(&self) -> bool {
        *self == Self::Exclusive
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryActivity {
    /// Unique per execution, not per operation: two PCs can retry the same plan.
    pub claim_id: Uuid,
    pub operation_id: Uuid,
    pub owner: String,
    /// Omitted legacy scope is always repository-exclusive, never a reader.
    #[serde(default, skip_serializing_if = "RepositoryAccess::is_exclusive")]
    pub access: RepositoryAccess,
    /// Durable agent identity (repository-owner-id), not a restart-random ID or
    /// proof that another instance's execution has stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<Uuid>,
}

impl RepositoryActivity {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.claim_id.is_nil()
            || self.operation_id.is_nil()
            || self.owner.trim().is_empty()
            || self.instance_id.is_some_and(|id| id.is_nil())
            || (self.access == RepositoryAccess::RestoreRead
                && (self.instance_id.is_none()
                    || self
                        .owner
                        .starts_with(crate::repository_recovery::RECOVERY_OWNER_PREFIX)))
        {
            return Err(StateError::Integrity(
                "invalid repository activity scope or identity".into(),
            ));
        }
        Ok(())
    }
}

fn key(id: Uuid) -> RemoteKey {
    RemoteKey::new(format!("maintenance/activity/{id}.enc"))
}

pub async fn repository_activities(
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
) -> Result<Vec<RepositoryActivity>> {
    // List the entire existing activity root, without a reader-only namespace
    // or filename filter. Old clients must see readers and block conservatively.
    let entries = provider
        .list_prefix(&RemoteKey::new("maintenance/activity"))
        .await?;
    if entries.len() > 1024 {
        return Err(StateError::Conflict(
            "too many repository activities; maintenance required".into(),
        ));
    }
    let mut output = Vec::new();
    let mut observed = HashSet::new();
    for entry in entries {
        let id = entry
            .key
            .as_str()
            .strip_prefix("maintenance/activity/")
            .and_then(|name| name.strip_suffix(".enc"))
            .and_then(|name| Uuid::parse_str(name).ok());
        if entry.is_prefix || id.is_none_or(|id| key(id) != entry.key || !observed.insert(id)) {
            return Err(StateError::Integrity(
                "ambiguous or invalid repository activity inventory".into(),
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
        activity.validate()?;
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
    current.validate()?;
    if current != *activity {
        return Err(StateError::Conflict(
            "repository activity belongs to another execution".into(),
        ));
    }
    provider.delete_object(&k).await?;
    // An acknowledged delete is not proof that this exact execution's claim
    // disappeared. Never retry it or report release until absence is confirmed.
    if provider.stat(&k).await?.is_some() {
        return Err(StateError::Conflict(
            "repository activity cleanup requires same-instance recovery; acknowledged delete did not confirm claim absence".into(),
        ));
    }
    Ok(())
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
        Self::acquire_scoped(
            provider,
            master,
            operation_id,
            owner,
            None,
            RepositoryAccess::Exclusive,
        )
        .await
    }

    /// Only read-only restores may request `RestoreRead`. An interrupted reader
    /// on another instance does not block downloading, but continues to fence
    /// every exclusive writer/deletion until its execution is safely resolved.
    pub async fn acquire_for_instance(
        provider: &'a dyn SharedStorageProvider,
        master: &'a MasterKey,
        operation_id: Uuid,
        owner: &str,
        instance_id: Uuid,
        access: RepositoryAccess,
    ) -> Result<Self> {
        Self::acquire_scoped(
            provider,
            master,
            operation_id,
            owner,
            Some(instance_id),
            access,
        )
        .await
    }

    async fn acquire_scoped(
        provider: &'a dyn SharedStorageProvider,
        master: &'a MasterKey,
        operation_id: Uuid,
        owner: &str,
        instance_id: Option<Uuid>,
        access: RepositoryAccess,
    ) -> Result<Self> {
        let activity = RepositoryActivity {
            claim_id: Uuid::new_v4(),
            operation_id,
            owner: owner.into(),
            access,
            instance_id,
        };
        activity.validate()?;
        for plan in crate::pending_deletions(provider, master).await? {
            if access != RepositoryAccess::Exclusive || plan.operation_id != operation_id {
                return Err(StateError::Conflict(format!(
                    "repository has unfinished deletion {}; resume that exact deletion first",
                    plan.plan_id
                )));
            }
        }
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
            let admitted = match access {
                RepositoryAccess::Exclusive => activities.len() == 1 && activities[0] == activity,
                RepositoryAccess::RestoreRead => {
                    activities.iter().filter(|other| **other == activity).count() == 1
                        && activities.iter().all(|other| {
                            other.access == RepositoryAccess::RestoreRead
                                && (other == &activity
                                    || (other.instance_id != activity.instance_id
                                        && other.owner != activity.owner))
                        })
                }
            };
            if !admitted {
                return Err(StateError::Conflict("another PC has active or interrupted Shared Storage work; finish or resolve it before continuing".into()));
            }
            // A previous execution can finish (or leave pending destructive
            // work) between our first pending read and publication of this
            // claim. Recheck after winning admission, not only before it.
            for plan in crate::pending_deletions(provider, master).await? {
                if access != RepositoryAccess::Exclusive || plan.operation_id != operation_id {
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

#[cfg(test)]
mod tests;
