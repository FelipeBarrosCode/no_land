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

pub async fn run_seal_with_session(
    agent: &StateAgent,
    session: &EphemeralRcloneSession,
    master: &MasterKey,
    mode: BackupMode,
) -> Result<SealRecord> {
    let (config_path, _session_guard) =
        write_guarded_ephemeral_session(&agent.config.paths.run_root, session)?;
    let storage = RcloneStorage::from_session(session, &config_path);
    run_seal(agent, &storage, master, mode).await
}
