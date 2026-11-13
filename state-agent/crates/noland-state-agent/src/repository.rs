use crate::StateAgent;
use noland_crypto::MasterKey;
use noland_state_core::*;
use noland_storage::{RepositoryGuard, SharedStorageProvider};
use uuid::Uuid;

pub(crate) struct AgentRepositoryGuard<'a> {
    remote: RepositoryGuard<'a>,
    // OS releases it only when this execution stops, including daemon crashes.
    _local: std::fs::File,
}

impl AgentRepositoryGuard<'_> {
    pub(crate) async fn release(self) -> Result<()> {
        self.remote.release().await
    }
}

pub(crate) fn owner(agent: &StateAgent, master: &MasterKey) -> Result<String> {
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
    Uuid::parse_str(&identity)
        .map_err(|_| StateError::Integrity("repository owner identity is corrupt".into()))?;
    Ok(blake3::keyed_hash(&master.0, identity.as_bytes())
        .to_hex()
        .to_string())
}

pub(crate) async fn acquire<'a>(
    agent: &StateAgent,
    provider: &'a dyn SharedStorageProvider,
    master: &'a MasterKey,
    id: Uuid,
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
    let owner = owner(agent, master)?;
    for activity in noland_storage::repository_activities(provider, master).await? {
        if activity.owner != owner {
            continue;
        }
        // This OS lock proves no prior execution using the same agent root
        // is alive. This also recovers claims from previews (no DB row) and
        // same-operation retries without assuming a timestamp means dead.
        noland_storage::clear_repository_activity(provider, master, &activity).await?;
    }
    Ok(AgentRepositoryGuard {
        remote: RepositoryGuard::acquire(provider, master, id, &owner).await?,
        _local: local,
    })
}

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
        assert_eq!(
            noland_storage::repository_activities(&storage, &master)
                .await
                .unwrap()
                .len(),
            1
        );
        let guard = acquire(&agent, &storage, &master, id).await.unwrap();
        guard.release().await.unwrap();
        assert!(noland_storage::repository_activities(&storage, &master)
            .await
            .unwrap()
            .is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}
