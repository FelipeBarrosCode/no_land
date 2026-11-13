use chrono::Utc;
use noland_crypto::MasterKey;
use noland_rclone_adapter::EphemeralRcloneSession;
use noland_state_core::*;
use noland_storage::{
    commit_checkpoint, commit_seal, write_guarded_ephemeral_session, RcloneStorage,
    SharedStorageProvider,
};
use uuid::Uuid;

use crate::backup::run_backup;
use crate::checkpoint::write_local_checkpoint;
use crate::reconcile::reconcile_app;
use crate::StateAgent;

pub fn deletion_allowed(agent: &StateAgent, kind: DeletionKind) -> Result<()> {
    if kind == DeletionKind::Force {
        return Ok(());
    }
    if let Some((_, state)) = agent.db.latest_seal()? {
        if state.allows_automatic_delete() {
            return Ok(());
        }
    }
    if !agent.db.list_dirty_apps()?.is_empty() {
        return Err(StateError::SealRequired);
    }
    Err(StateError::SealRequired)
}

pub async fn run_seal(
    agent: &StateAgent,
    provider: &dyn SharedStorageProvider,
    master: &MasterKey,
    mode: BackupMode,
) -> Result<SealRecord> {
    let op_id = Uuid::new_v4();
    let mut op = OperationRecord {
        operation_id: op_id,
        kind: "seal".into(),
        app_id: None,
        state: SealState::Requested.as_str().into(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        last_error: None,
        detail_json: serde_json::json!({}),
    };
    agent.db.upsert_operation(&op)?;

    op.state = SealState::Reconciling.as_str().into();
    agent.db.upsert_operation(&op)?;
    for dirty in agent.db.list_dirty_apps()? {
        reconcile_app(agent, &dirty.app_id)?;
    }

    op.state = SealState::BackingUpDirtyApps.as_str().into();
    agent.db.upsert_operation(&op)?;
    let mut commits = Vec::new();
    for dirty in agent.db.list_dirty_apps()? {
        let manifest = run_backup(
            agent,
            &dirty.app_id,
            mode,
            BackupPerformanceMode::Balanced,
            provider,
            master,
            None,
        )
        .await?;
        commits.push(SealAppCommit {
            app_id: dirty.app_id,
            bundle_id: manifest.bundle_id,
            commit_id: manifest.commit_id,
        });
    }
    // Also include already committed clean apps so the seal is complete.
    for app in agent.db.list_apps()? {
        if commits.iter().any(|c| c.app_id == app.app_id) {
            continue;
        }
        if let Some((commit_id, bundle_id, _)) = agent.db.latest_commit(&app.app_id)? {
            commits.push(SealAppCommit {
                app_id: app.app_id,
                bundle_id,
                commit_id,
            });
        }
    }

    // Child transfers release admission between apps. Protect final validation
    // and seal publication as one repository operation: a delete in that gap
    // must make the seal fail, never authorize destroying its last local copy.
    let guard = crate::repository::acquire(agent, provider, master, op_id).await?;
    let result = async {
        for commit in &commits {
            let manifest = noland_storage::read_committed_manifest(
                provider,
                master,
                &commit.app_id,
                commit.bundle_id,
            )
            .await?;
            if manifest.commit_id != commit.commit_id {
                return Err(StateError::Conflict(
                    "a seal backup changed before final publication".into(),
                ));
            }
        }
        op.state = SealState::Checkpointing.as_str().into();
        agent.db.upsert_operation(&op)?;
        let checkpoint = write_local_checkpoint(agent)?;
        commit_checkpoint(provider, master, &checkpoint).await?;

        let seal = SealRecord {
            seal_id: Uuid::new_v4(),
            instance_id: agent.config.instance_id,
            image_id: agent.config.image_id.clone(),
            sealed_at: Utc::now(),
            app_bundle_commits: commits,
            checkpoint_id: Some(checkpoint.checkpoint_id),
            state: "complete".into(),
        };
        op.state = SealState::UploadingSeal.as_str().into();
        agent.db.upsert_operation(&op)?;
        commit_seal(provider, master, &seal).await?;
        op.state = SealState::CommittingSeal.as_str().into();
        agent.db.upsert_operation(&op)?;
        agent.db.record_seal(&seal, SealState::Sealed)?;
        op.state = SealState::Sealed.as_str().into();
        agent.db.upsert_operation(&op)?;
        Ok(seal)
    }
    .await;
    guard.release().await?;
    result
}

pub async fn run_seal_with_session(
    agent: &StateAgent,
    session: &EphemeralRcloneSession,
    master: &MasterKey,
    mode: BackupMode,
) -> Result<SealRecord> {
    let (config_path, _session_guard) =
        write_guarded_ephemeral_session(&agent.config.paths.run_root, session)?;
    let storage = RcloneStorage::try_from_session(session, &config_path)?;
    run_seal(agent, &storage, master, mode).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn deletion_of_a_clean_app_backup_prevents_seal_publication() {
        let root = std::env::current_dir()
            .unwrap()
            .join(format!("target/seal-delete-{}", Uuid::new_v4()));
        let agent = StateAgent::boot(crate::AgentConfig::isolated(root.clone())).unwrap();
        let app = AppIdentity::new(AppId::desktop("fixture"), "Fixture");
        agent.db.upsert_app(&app).unwrap();
        let storage = noland_storage::LocalStorage::new(root.join("cloud"));
        let master = MasterKey::generate();
        let manifest = BundleManifest::new(
            ManifestApp::from(&app),
            ManifestSource {
                instance_id: agent.config.instance_id,
                image_id: "fixture".into(),
                captured_at: Utc::now(),
            },
            BackupMode::CompleteApplication,
        );
        noland_storage::commit_bundle(&storage, &master, &manifest, &[], None)
            .await
            .unwrap();
        agent
            .db
            .record_commit(
                manifest.commit_id,
                &app.app_id,
                manifest.bundle_id,
                "fixture",
                None,
                CommitVisibility::Committed,
            )
            .unwrap();
        noland_storage::delete_bundle(
            &storage,
            &master,
            &noland_storage::BundleDeletion {
                app_id: app.app_id.clone(),
                bundle_id: manifest.bundle_id,
                commit_id: manifest.commit_id,
            },
        )
        .await
        .unwrap();
        assert!(
            run_seal(&agent, &storage, &master, BackupMode::CompleteApplication)
                .await
                .is_err()
        );
        assert!(
            agent.db.latest_seal().unwrap().is_none(),
            "an absent/deleted clean backup can never authorize automatic destruction"
        );
        assert!(noland_storage::repository_activities(&storage, &master)
            .await
            .unwrap()
            .is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}
