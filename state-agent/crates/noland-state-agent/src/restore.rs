use noland_crypto::MasterKey;
use noland_rclone_adapter::EphemeralRcloneSession;
use noland_restore::{
    apply_restore, cleanup_restore, download_and_verify, materialize_tree, prepare_restore,
};
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

    // Dynamic roots such as Steam libraries may appear after the agent starts.
    // Refresh them before resolving the portable logical roots in the manifest.
    agent.discover()?;

    let plan = prepare_restore(
        &storage,
        master,
        &agent.config.paths,
        app_id,
        bundle_id,
        mode,
    )
    .await;
    let result = match plan {
        Ok(plan) => {
            let restore_result = async {
                let manifest_app = &plan.manifest.app;
                agent.db.upsert_app(&AppIdentity {
                    app_id: manifest_app.app_id.clone(),
                    display_name: manifest_app.display_name.clone(),
                    canonical_executable: manifest_app.canonical_executable.clone(),
                    desktop_entry_id: manifest_app.desktop_entry_id.clone(),
                    steam_app_id: manifest_app.steam_app_id,
                    launcher: manifest_app.launcher,
                    aliases: manifest_app.aliases.clone(),
                    identity_confidence: 1.0,
                    icon_path: manifest_app.icon_path.clone(),
                })?;

                let index = read_pack_index(&storage, master, app_id, bundle_id).await?;
                download_and_verify(&storage, master, &plan, &index).await?;
                let roots = agent.roots.lock().clone();
                materialize_tree(&plan, &roots)?;
                apply_restore(&plan, &roots, Some(&agent.db))
            }
            .await;

            match restore_result {
                Ok(rollback) => cleanup_restore(&plan, rollback),
                Err(error) => {
                    if let Err(cleanup_error) = cleanup_restore(&plan, None) {
                        tracing::warn!(
                            restore_id = %plan.restore_id,
                            %cleanup_error,
                            "failed to clean restore staging after restore error"
                        );
                    }
                    Err(error)
                }
            }
        }
        Err(error) => Err(error),
    };
    let _ = shred_ephemeral_session(&agent.config.paths.run_root, &session.operation_id);
    result
}
