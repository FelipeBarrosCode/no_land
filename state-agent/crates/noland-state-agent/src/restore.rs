use noland_crypto::MasterKey;
use noland_rclone_adapter::EphemeralRcloneSession;
use noland_restore::{apply_restore, download_and_verify, materialize_tree, prepare_restore};
use noland_state_core::*;
use noland_storage::{
    read_pack_index, shred_ephemeral_session, write_ephemeral_session, RcloneStorage,
};

use crate::StateAgent;

pub async fn run_restore_with_session(
    agent: &StateAgent,
    app_id: &AppId,
    bundle_id: uuid::Uuid,
    mode: RestoreMode,
    session: &EphemeralRcloneSession,
    master: &MasterKey,
) -> Result<()> {
    let config_path = write_ephemeral_session(&agent.config.paths.run_root, session)?;
    let storage = RcloneStorage::from_session(session, &config_path);
    let plan = prepare_restore(
        &storage,
        master,
        &agent.config.paths,
        app_id,
        bundle_id,
        mode,
    )
    .await;
    let result = async {
        let plan = plan?;
        let index = read_pack_index(&storage, master, app_id, bundle_id).await?;
        download_and_verify(&storage, master, &plan, &index).await?;
        let roots = agent.roots.lock().clone();
        materialize_tree(&plan, &roots)?;
        apply_restore(&plan, &roots, Some(&agent.db))?;
        Ok(())
    }
    .await;
    let _ = shred_ephemeral_session(&agent.config.paths.run_root, &session.operation_id);
    result
}
