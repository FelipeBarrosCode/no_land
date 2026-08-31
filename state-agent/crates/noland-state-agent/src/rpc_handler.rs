use std::sync::Arc;

use async_trait::async_trait;
use noland_crypto::MasterKey;
use noland_rclone_adapter::EphemeralRcloneSession;
use noland_rpc::{HealthStatus, RpcHandler, RpcRequest};
use noland_state_core::*;
use noland_storage::{
    load_catalog, shred_ephemeral_session, write_ephemeral_session, RcloneStorage,
    SharedStorageProvider,
};
use serde_json::json;

use crate::StateAgent;

const AGENT_API_VERSION: u64 = 2;

pub struct AgentRpc(pub Arc<StateAgent>);

#[async_trait]
impl RpcHandler for AgentRpc {
    async fn handle(&self, request: &RpcRequest) -> Result<serde_json::Value> {
        let agent = &self.0;
        match request.method.as_str() {
            "GetHealth" => {
                let unfinished = agent.db.unfinished_operations()?.len();
                let observer = agent.observer.status(agent.hub.queue.dropped());
                let mut health = serde_json::to_value(HealthStatus {
                    status: if observer.state == crate::observer::ObserverCapabilityState::Active {
                        "ok".into()
                    } else {
                        "degraded".into()
                    },
                    image_id: agent.config.image_id.clone(),
                    instance_id: agent.config.instance_id.to_string(),
                    socket: agent.config.paths.rpc_socket.display().to_string(),
                    metrics: agent.metrics.snapshot(),
                    unfinished_operations: unfinished,
                })?;
                let fields = health
                    .as_object_mut()
                    .expect("HealthStatus serializes as an object");
                fields.insert("agent_api_version".into(), AGENT_API_VERSION.into());
                fields.insert("observer".into(), serde_json::to_value(observer)?);
                Ok(health)
            }
            "ListApps" => {
                let _ = agent.discover();
                let apps = noland_discovery::filter_backup_candidates(agent.db.list_apps()?);
                let apps = apps
                    .iter()
                    .map(serialize_app_identity)
                    .collect::<Result<Vec<_>>>()?;
                Ok(serde_json::Value::Array(apps))
            }
            "GetAppDetails" => {
                let app_id = AppId(req_str(&request.params, "app_id")?);
                let app = agent
                    .db
                    .get_app(&app_id)?
                    .ok_or_else(|| StateError::NotFound(app_id.to_string()))?;
                serialize_app_identity(&app)
            }
            "GetAppPaths" => {
                let app_id = AppId(req_str(&request.params, "app_id")?);
                let rows = agent.db.associations_for_app(&app_id)?;
                let payload: Vec<_> = rows
                    .into_iter()
                    .map(|(path, assoc)| {
                        json!({
                            "path": path.canonical_path,
                            "logical_root": path.logical_root,
                            "relative_path": path.relative_path,
                            "confidence": assoc.confidence,
                            "persistence_class": assoc.persistence_class,
                            "semantic_role": assoc.semantic_role,
                            "evidence": assoc.evidence.iter().map(|e| e.kind).collect::<Vec<_>>(),
                        })
                    })
                    .collect();
                Ok(json!(payload))
            }
            "GetDirtyApps" => Ok(serde_json::to_value(agent.db.list_dirty_apps()?)?),
            "StartBackup" => {
                let app_id_raw = request
                    .params
                    .get("app_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("*");
                let mode = BackupMode::parse(
                    request
                        .params
                        .get("mode")
                        .and_then(|v| v.as_str())
                        .unwrap_or("personal_state"),
                );
                let session = parse_session(&request.params)?;
                let master = master_from_params(&request.params, agent)?;
                let _ = agent.discover();
                let _ = agent.process_events();
                let op_id = uuid::Uuid::new_v4();
                let op = OperationRecord {
                    operation_id: op_id,
                    kind: "backup".into(),
                    app_id: if app_id_raw == "*" {
                        None
                    } else {
                        Some(AppId(app_id_raw.into()))
                    },
                    state: BackupState::Queued.as_str().into(),
                    created_at: chrono::Utc::now(),
                    updated_at: chrono::Utc::now(),
                    last_error: None,
                    detail_json: json!({"session_operation": session.operation_id}),
                };
                agent.db.upsert_operation(&op)?;
                let result = if app_id_raw == "*" || app_id_raw == "_all" {
                    crate::backup::run_backup_all_with_session(agent, mode, &session, &master)
                        .await
                        .map(|manifests| {
                            json!({
                                "operation_id": op_id,
                                "state": "COMPLETED",
                                "count": manifests.len(),
                                "bundle_ids": manifests.iter().map(|m| m.bundle_id).collect::<Vec<_>>(),
                            })
                        })
                } else {
                    crate::backup::run_backup_with_session(
                        agent,
                        &AppId(app_id_raw.into()),
                        mode,
                        &session,
                        &master,
                    )
                    .await
                    .map(|manifest| {
                        json!({
                            "operation_id": op_id,
                            "state": "COMPLETED",
                            "bundle_id": manifest.bundle_id,
                            "commit_id": manifest.commit_id,
                        })
                    })
                };
                match result {
                    Ok(value) => Ok(value),
                    Err(err) => {
                        let mut failed = op;
                        failed.state = BackupState::Failed.as_str().into();
                        failed.last_error = Some(err.to_string());
                        agent.db.upsert_operation(&failed)?;
                        Err(err)
                    }
                }
            }
            "StartSeal" => {
                let session = parse_session(&request.params)?;
                let master = master_from_params(&request.params, agent)?;
                let mode = BackupMode::parse(
                    request
                        .params
                        .get("mode")
                        .and_then(|v| v.as_str())
                        .unwrap_or("personal_state"),
                );
                let seal =
                    crate::seal::run_seal_with_session(agent, &session, &master, mode).await?;
                Ok(serde_json::to_value(seal)?)
            }
            "StartCheckpoint" => {
                let checkpoint = crate::checkpoint::write_local_checkpoint(agent)?;
                if let Ok(session) = parse_session(&request.params) {
                    let master = master_from_params(&request.params, agent)?;
                    let config = write_ephemeral_session(&agent.config.paths.run_root, &session)?;
                    let storage = RcloneStorage::from_session(&session, &config);
                    let _ = noland_storage::commit_checkpoint(&storage, &master, &checkpoint).await;
                    let _ = shred_ephemeral_session(
                        &agent.config.paths.run_root,
                        &session.operation_id,
                    );
                }
                Ok(json!({"checkpoint_id": checkpoint.checkpoint_id}))
            }
            "StartRestore" => {
                let app_id = AppId(req_str(&request.params, "app_id")?);
                let bundle_id = uuid::Uuid::parse_str(&req_str(&request.params, "bundle_id")?)
                    .map_err(|e| StateError::Invalid(e.to_string()))?;
                let mode = match request
                    .params
                    .get("mode")
                    .and_then(|v| v.as_str())
                    .unwrap_or("personal_state")
                {
                    "complete_application" | "complete" => RestoreMode::CompleteApplication,
                    "custom" => RestoreMode::Custom,
                    _ => RestoreMode::PersonalState,
                };
                let session = parse_session(&request.params)?;
                let master = master_from_params(&request.params, agent)?;
                crate::restore::run_restore_with_session(
                    agent, &app_id, bundle_id, mode, &session, &master,
                )
                .await?;
                Ok(json!({"state": "COMPLETED", "app_id": app_id.as_str(), "bundle_id": bundle_id}))
            }
            "GetBackupStatus" | "GetRestoreStatus" | "GetSealStatus" => {
                let id = request
                    .params
                    .get("operation_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| StateError::Invalid("operation_id required".into()))?;
                let uuid =
                    uuid::Uuid::parse_str(id).map_err(|e| StateError::Invalid(e.to_string()))?;
                let op = agent
                    .db
                    .get_operation(uuid)?
                    .ok_or_else(|| StateError::NotFound(id.into()))?;
                Ok(serde_json::to_value(op)?)
            }
            "CancelBackup" | "CancelRestore" => Ok(json!({"cancelled": true})),
            "ListLocalCommits" => {
                let app_id = AppId(req_str(&request.params, "app_id")?);
                Ok(json!(agent.db.latest_commit(&app_id)?))
            }
            "ListCloudCatalog" => {
                let session = parse_session(&request.params)?;
                let master = master_from_params(&request.params, agent)?;
                let config = write_ephemeral_session(&agent.config.paths.run_root, &session)?;
                let storage = RcloneStorage::from_session(&session, &config);
                let catalog = load_catalog(&storage, &master).await;
                let _ =
                    shred_ephemeral_session(&agent.config.paths.run_root, &session.operation_id);
                Ok(serde_json::to_value(catalog?)?)
            }
            "GetStorageHealth" => {
                if let Ok(session) = parse_session(&request.params) {
                    let config = write_ephemeral_session(&agent.config.paths.run_root, &session)?;
                    let storage = RcloneStorage::from_session(&session, &config);
                    let health = storage.health_check().await?;
                    let _ = shred_ephemeral_session(
                        &agent.config.paths.run_root,
                        &session.operation_id,
                    );
                    return Ok(serde_json::to_value(health)?);
                }
                Ok(json!({"ok": true, "provider": "none"}))
            }
            "ReconcileApp" => {
                let app_id = AppId(req_str(&request.params, "app_id")?);
                let n = crate::reconcile::reconcile_app(agent, &app_id)?;
                Ok(json!({"reconciled": n}))
            }
            "SetManualPathBinding" => {
                let app_id = AppId(req_str(&request.params, "app_id")?);
                let path = req_str(&request.params, "path")?;
                let engine = noland_attribution::AttributionEngine::new(
                    &agent.db,
                    agent.roots.lock().clone(),
                    agent.config.paths.clone(),
                );
                let assoc = engine.bind_manual(&app_id, std::path::Path::new(&path))?;
                Ok(json!({"confidence": assoc.confidence}))
            }
            "SetPathExclusion" => {
                let path = req_str(&request.params, "path")?;
                let app_id = request
                    .params
                    .get("app_id")
                    .and_then(|v| v.as_str())
                    .map(|s| AppId(s.into()));
                agent
                    .db
                    .set_path_policy(&path, app_id.as_ref(), "exclude")?;
                Ok(json!({"excluded": path}))
            }
            other => Err(StateError::Invalid(format!("unknown method {other}"))),
        }
    }
}

fn serialize_app_identity(app: &AppIdentity) -> Result<serde_json::Value> {
    let mut value = serde_json::to_value(app)?;
    if let Some(method) = app.launch_method() {
        value
            .as_object_mut()
            .expect("AppIdentity serializes as an object")
            .insert("launch_method".into(), serde_json::to_value(method)?);
    }
    Ok(value)
}

fn req_str(params: &serde_json::Value, key: &str) -> Result<String> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| StateError::Invalid(format!("{key} required")))
}

fn parse_session(params: &serde_json::Value) -> Result<EphemeralRcloneSession> {
    let value = params
        .get("session")
        .cloned()
        .ok_or_else(|| StateError::Invalid("session required".into()))?;
    serde_json::from_value(value).map_err(|e| StateError::Invalid(e.to_string()))
}

fn master_from_params(params: &serde_json::Value, agent: &StateAgent) -> Result<MasterKey> {
    if let Some(hex) = params.get("master_key_hex").and_then(|v| v.as_str()) {
        let bytes = decode_hex_key(hex)?;
        return MasterKey::from_slice(&bytes);
    }
    agent
        .master_key
        .lock()
        .clone()
        .ok_or_else(|| StateError::Crypto("master key not installed and not provided".into()))
}

fn decode_hex_key(hex: &str) -> Result<Vec<u8>> {
    let hex = hex.trim();
    if hex.len() != 64 {
        return Err(StateError::Crypto(
            "master_key_hex must be 32 bytes hex".into(),
        ));
    }
    (0..32)
        .map(|i| {
            u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
                .map_err(|_| StateError::Crypto("invalid master_key_hex".into()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_identity_rpc_value_includes_launch_metadata() {
        let mut app = AppIdentity::new(AppId::steam(480), "Spacewar");
        app.aliases = vec!["Steam Test".into()];
        app.desktop_entry_id = Some("steam-480.desktop".into());
        app.steam_app_id = Some(480);
        app.launcher = Some(LauncherKind::Steam);
        app.canonical_executable = Some("/games/spacewar".into());
        app.icon_path = Some("/icons/spacewar.png".into());

        let value = serialize_app_identity(&app).unwrap();
        assert_eq!(value["app_id"], "steam:480");
        assert_eq!(value["display_name"], "Spacewar");
        assert_eq!(value["aliases"], serde_json::json!(["Steam Test"]));
        assert_eq!(value["desktop_entry_id"], "steam-480.desktop");
        assert_eq!(value["steam_app_id"], 480);
        assert_eq!(value["launcher"], "steam");
        assert_eq!(value["canonical_executable"], "/games/spacewar");
        assert_eq!(value["icon_path"], "/icons/spacewar.png");
        assert_eq!(value["launch_method"], "steam");
    }
}
