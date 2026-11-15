use crate::StateAgent;
use noland_crypto::MasterKey;
use noland_state_core::*;
use noland_storage::{RepositoryAccess, RepositoryGuard, SharedStorageProvider};
use uuid::Uuid;

const CLEANUP_CODE: &str = "REPOSITORY_CLEANUP_REQUIRED";
const CLEANUP_DETAIL: &str = "Repository claim cleanup was not confirmed. Automatic recovery requires the same instance after local execution stops; otherwise use explicitly authorized recovery. Do not automatically retry cloud deletion.";

fn cleanup_error() -> StateError {
    StateError::Storage(format!("{CLEANUP_CODE}: {CLEANUP_DETAIL}"))
}

pub(crate) struct AgentRepositoryGuard<'a> {
    remote: Option<RepositoryGuard<'a>>,
    agent: &'a StateAgent,
    operation_id: Uuid,
    instance_id: Uuid,
    access: RepositoryAccess,
    finalized: bool,
    // OS releases it only when this execution stops, including daemon crashes.
    _local: std::fs::File,
}

impl AgentRepositoryGuard<'_> {
    pub(crate) async fn release(mut self) -> Result<()> {
        // Keep self (and its OS lock) alive across the awaited remote release.
        // Dropping this future is uncertain cleanup, not a successful release.
        let result = self
            .remote
            .take()
            .expect("unreleased repository guard")
            .release()
            .await;
        self.record(
            if result.is_ok() {
                "RELEASED"
            } else {
                "CLEANUP_REQUIRED"
            },
            result.as_ref().err().map(|_| "release_failed"),
        );
        self.finalized = true;
        // Preview/RPC callers may display this error. Some provider stat/control
        // failures echo requests, so never return their raw cleanup stderr.
        result.map_err(|_| cleanup_error())
    }

    /// Cleanup is a separate outcome from the transfer. A durable restore,
    /// committed backup/seal, or completed deletion must not become retryable
    /// just because releasing its claim failed; nor may cleanup hide its error.
    pub(crate) async fn finish<T>(self, result: Result<T>) -> Result<T> {
        let _ = self.release().await;
        result
    }

    fn record(&self, status: &str, reason: Option<&str>) {
        record(
            self.agent,
            self.operation_id,
            self.instance_id,
            self.access,
            status,
            reason,
        );
    }
}

impl Drop for AgentRepositoryGuard<'_> {
    fn drop(&mut self) {
        if !self.finalized {
            // Cancellation drops transfer workers before their borrowed guard.
            // Do not start detached credential-using work here, or clear a
            // claim until the OS lock proves all of this execution has stopped.
            self.record("CLEANUP_REQUIRED", Some("execution_stopped_before_release"));
        }
    }
}

struct Owner {
    instance_id: Uuid,
    hash: String,
}

fn identity(agent: &StateAgent, master: &MasterKey) -> Result<Owner> {
    use std::io::Write;
    let path = agent.config.paths.state_root.join("repository-owner-id");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(&path) {
        Ok(mut file) => {
            file.write_all(Uuid::new_v4().to_string().as_bytes())?;
            file.sync_all()?;
            std::fs::File::open(&agent.config.paths.state_root)?.sync_all()?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let identity = std::fs::read_to_string(path)?;
    let instance_id = Uuid::parse_str(&identity)
        .map_err(|_| StateError::Integrity("repository owner identity is corrupt".into()))?;
    if instance_id.is_nil() {
        return Err(StateError::Integrity(
            "repository owner identity is corrupt".into(),
        ));
    }
    Ok(Owner {
        instance_id,
        // Preserve the exact legacy hash, including the original UUID spelling.
        hash: blake3::keyed_hash(&master.0, identity.as_bytes())
            .to_hex()
            .to_string(),
    })
}

pub(crate) fn owner(agent: &StateAgent, master: &MasterKey) -> Result<String> {
    Ok(identity(agent, master)?.hash)
}

pub(crate) async fn acquire<'a>(
    agent: &'a StateAgent,
    provider: &'a dyn SharedStorageProvider,
    master: &'a MasterKey,
    id: Uuid,
) -> Result<AgentRepositoryGuard<'a>> {
    acquire_scoped(agent, provider, master, id, RepositoryAccess::Exclusive).await
}

pub(crate) async fn acquire_restore<'a>(
    agent: &'a StateAgent,
    provider: &'a dyn SharedStorageProvider,
    master: &'a MasterKey,
    id: Uuid,
) -> Result<AgentRepositoryGuard<'a>> {
    acquire_scoped(agent, provider, master, id, RepositoryAccess::RestoreRead).await
}

async fn acquire_scoped<'a>(
    agent: &'a StateAgent,
    provider: &'a dyn SharedStorageProvider,
    master: &'a MasterKey,
    id: Uuid,
    access: RepositoryAccess,
) -> Result<AgentRepositoryGuard<'a>> {
    let local = noland_snapshot::SourcePath::new(
        &agent
            .config
            .paths
            .state_root
            .join("repository-admission.lock"),
    )?
    .open_or_create_private()?;
    local.try_lock().map_err(|_| {
        StateError::Conflict("another local execution owns repository admission".into())
    })?;
    let owner = identity(agent, master)?;
    for activity in noland_storage::repository_activities(provider, master).await? {
        if activity.owner != owner.hash
            || activity
                .instance_id
                .is_some_and(|id| id != owner.instance_id)
        {
            continue;
        }
        // This OS lock proves no prior execution using the same agent root
        // is alive. This also recovers claims from previews (no DB row) and
        // same-operation retries without assuming a timestamp means dead.
        record(
            agent,
            activity.operation_id,
            owner.instance_id,
            activity.access,
            "CLEANUP_REQUIRED",
            Some("owner_recovery_in_progress"),
        );
        if noland_storage::clear_repository_activity(provider, master, &activity)
            .await
            .is_err()
        {
            record(
                agent,
                activity.operation_id,
                owner.instance_id,
                activity.access,
                "CLEANUP_REQUIRED",
                Some("owner_recovery_failed"),
            );
            return Err(cleanup_error());
        }
        record(
            agent,
            activity.operation_id,
            owner.instance_id,
            activity.access,
            "RECOVERED",
            None,
        );
    }
    let mut guard = AgentRepositoryGuard {
        remote: None,
        agent,
        operation_id: id,
        instance_id: owner.instance_id,
        access,
        finalized: false,
        _local: local,
    };
    guard.record("ACQUIRING", None);
    // A dropped/failed acquire may have published a claim, even if the provider
    // never returned an acknowledgement. Leave that uncertainty fail-closed.
    let remote = RepositoryGuard::acquire_for_instance(
        provider,
        master,
        id,
        &owner.hash,
        owner.instance_id,
        access,
    )
    .await;
    guard.remote = Some(match remote {
        Ok(remote) => remote,
        Err(error) => {
            // Admission can reject us after publishing and then removing its
            // candidate claim. Confirm absence rather than falsely reporting a
            // clean rejection when that one cleanup delete was uncertain.
            if noland_storage::repository_activities(provider, master)
                .await
                .is_ok_and(|activities| {
                    !activities.iter().any(|activity| {
                        activity.operation_id == id
                            && activity.owner == owner.hash
                            && activity.instance_id == Some(owner.instance_id)
                    })
                })
            {
                guard.record("NOT_ADMITTED", None);
                guard.finalized = true;
            }
            return Err(error);
        }
    });
    guard.record("ADMITTED", None);
    Ok(guard)
}

/// Only fixed strings/UUIDs are archived: provider errors may contain credentials
/// or private rclone configuration. The original transfer error stays separate.
fn record(
    agent: &StateAgent,
    operation_id: Uuid,
    instance_id: Uuid,
    access: RepositoryAccess,
    status: &str,
    reason: Option<&str>,
) {
    let cleanup_required = status == "CLEANUP_REQUIRED";
    let detail = serde_json::json!({
        "version": 1,
        "operation_id": operation_id,
        "instance_id": instance_id,
        "access": access,
        "status": status,
        "cleanup_required": cleanup_required,
        "code": cleanup_required.then_some(CLEANUP_CODE),
        "reason": reason,
        "detail": cleanup_required.then_some(CLEANUP_DETAIL),
    });
    let persist = (|| -> Result<()> {
        let operation = agent.db.get_operation(operation_id)?;
        let parent = operation
            .as_ref()
            .and_then(|op| op.detail_json.get("parent_operation_id"))
            .and_then(serde_json::Value::as_str)
            .and_then(|id| Uuid::parse_str(id).ok());
        // A preview can own a claim before it has an OperationRecord. Keep its
        // cleanup report under a separate deterministic ID, never occupying an
        // ID that a later confirmed deletion/backup must be allowed to submit.
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"noland-repository-cleanup-diagnostic-v1\0");
        hasher.update(operation_id.as_bytes());
        let mut bytes = [0; 16];
        bytes.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
        let cleanup_id = Uuid::from_bytes(bytes);
        let mut fallback = if operation.is_none() || status == "RECOVERED" {
            agent.db.get_operation(cleanup_id)?
        } else {
            None
        };
        let mut operation = match operation.or_else(|| fallback.take()) {
            Some(operation) => operation,
            None if cleanup_required => OperationRecord {
                operation_id: cleanup_id,
                kind: "repository_cleanup".into(),
                app_id: None,
                state: "INTERRUPTED".into(),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                last_error: None,
                detail_json: serde_json::json!({}),
            },
            None => return Ok(()),
        };
        persist_detail(agent, &mut operation, &detail)?;
        if let Some(mut preview_diagnostic) = fallback {
            persist_detail(agent, &mut preview_diagnostic, &detail)?;
        }
        if let Some(parent) = parent.filter(|parent| *parent != operation_id) {
            if let Some(mut parent) = agent.db.get_operation(parent)? {
                persist_detail(agent, &mut parent, &detail)?;
            }
        }
        Ok(())
    })();
    if persist.is_err() {
        tracing::error!(%operation_id, "could not persist repository admission diagnostics");
    }
    if cleanup_required {
        tracing::warn!(%operation_id, %instance_id, ?access, reason, "repository admission cleanup requires same-instance recovery");
    }
}

fn persist_detail(
    agent: &StateAgent,
    operation: &mut OperationRecord,
    detail: &serde_json::Value,
) -> Result<()> {
    if !operation.detail_json.is_object() {
        operation.detail_json = serde_json::json!({});
    }
    operation.detail_json["repository_admission"] = detail.clone();
    operation.updated_at = chrono::Utc::now();
    agent.db.upsert_operation(operation)?;
    let progress = agent.db.get_operation_progress(operation.operation_id)?;
    if let Some(mut progress) = progress.or_else(|| {
        (detail["cleanup_required"] == true).then(|| OperationProgress::new(&operation.state, 0))
    }) {
        if !progress.detail_json.is_object() {
            progress.detail_json = serde_json::json!({});
        }
        progress.detail_json["repository_admission"] = detail.clone();
        agent
            .db
            .set_operation_progress(operation.operation_id, Some(&progress))?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "repository_tests.rs"]
pub(crate) mod cleanup_tests;

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn local_os_lock_protects_active_workers_and_recovers_abandoned_preview_claims() {
        let root = std::env::current_dir()
            .unwrap()
            .join(format!("target/repository-owner-{}", Uuid::new_v4()));
        let agent = StateAgent::boot(crate::AgentConfig::isolated(root.clone())).unwrap();
        let storage = noland_storage::LocalStorage::new(root.join("cloud"));
        let master = MasterKey::generate();
        let id = Uuid::new_v4();
        let guard = acquire(&agent, &storage, &master, id).await.unwrap();
        assert!(
            acquire(&agent, &storage, &master, id).await.is_err(),
            "same operation is not proof that a worker has stopped"
        );
        drop(guard); // e.g. a cancelled preview: no OperationRecord exists
        assert!(agent.db.get_operation(id).unwrap().is_none());
        let preview_diagnostic = agent
            .db
            .recent_operations(10)
            .unwrap()
            .into_iter()
            .find(|op| op.kind == "repository_cleanup")
            .unwrap();
        assert_eq!(
            preview_diagnostic.detail_json["repository_admission"]["status"],
            "CLEANUP_REQUIRED"
        );
        agent
            .db
            .upsert_operation(&OperationRecord {
                operation_id: id,
                kind: "delete".into(),
                app_id: None,
                state: "QUEUED".into(),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                last_error: None,
                detail_json: serde_json::json!({}),
            })
            .unwrap();
        assert_eq!(
            noland_storage::repository_activities(&storage, &master)
                .await
                .unwrap()
                .len(),
            1
        );
        let guard = acquire(&agent, &storage, &master, id).await.unwrap();
        guard.release().await.unwrap();
        assert_eq!(
            agent
                .db
                .get_operation(preview_diagnostic.operation_id)
                .unwrap()
                .unwrap()
                .detail_json["repository_admission"]["status"],
            "RECOVERED"
        );
        assert!(noland_storage::repository_activities(&storage, &master)
            .await
            .unwrap()
            .is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}
