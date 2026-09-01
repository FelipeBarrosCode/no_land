use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use noland_crypto::MasterKey;
use noland_rclone_adapter::EphemeralRcloneSession;
use noland_rpc::{HealthStatus, RpcHandler, RpcRequest};
use noland_state_core::*;
use noland_storage::{
    load_catalog, write_guarded_ephemeral_session, RcloneStorage, SharedStorageProvider,
};
use serde_json::json;

use crate::StateAgent;

const AGENT_API_VERSION: u64 = 9;

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

                let all_apps = app_id_raw == "*" || app_id_raw == "_all";
                let app_id = (!all_apps).then(|| AppId(app_id_raw.into()));
                let task_agent = Arc::clone(agent);
                let terminated_agent = Arc::clone(agent);
                let operation_manager = agent.operations.clone();
                let total_started = Instant::now();
                let spawned = operation_manager.spawn(
                    op_id,
                    async move {
                        let discovery_started = Instant::now();
                        let discovery = task_agent
                            .discover()
                            .and_then(|_| task_agent.process_events().map(|_| ()));
                        if let Err(error) = discovery {
                            mark_operation_failed(
                                &task_agent,
                                op_id,
                                error.to_string(),
                                total_started,
                            );
                            return;
                        }
                        record_discovery_duration(&task_agent, op_id, discovery_started);

                        let result = if all_apps {
                            crate::backup::run_backup_all_with_session(
                                &task_agent,
                                mode,
                                &session,
                                &master,
                            )
                            .await
                            .map(|manifests| {
                                let bundle_ids = manifests
                                    .iter()
                                    .map(|manifest| manifest.bundle_id)
                                    .collect::<Vec<_>>();
                                (manifests.len(), bundle_ids)
                            })
                        } else {
                            crate::backup::run_backup_with_session(
                                &task_agent,
                                app_id.as_ref().expect("single-app backup has an app id"),
                                mode,
                                &session,
                                &master,
                                Some(op_id),
                            )
                            .await
                            .map(|manifest| (1, vec![manifest.bundle_id]))
                        };

                        match result {
                            Ok((count, bundle_ids)) if all_apps => {
                                mark_all_backup_completed(
                                    &task_agent,
                                    op_id,
                                    count,
                                    &bundle_ids,
                                    total_started,
                                );
                            }
                            Ok(_) => {}
                            Err(error) => {
                                mark_operation_failed(
                                    &task_agent,
                                    op_id,
                                    error.to_string(),
                                    total_started,
                                );
                            }
                        }
                    },
                    move |error| {
                        mark_operation_failed(&terminated_agent, op_id, error, total_started);
                    },
                );
                if !spawned {
                    return Err(StateError::Invalid(format!(
                        "operation {op_id} is already running"
                    )));
                }

                Ok(json!({
                    "operation_id": op_id,
                    "state": BackupState::Queued.as_str(),
                    "stage": BackupState::Queued.as_str(),
                }))
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
                let seal = agent
                    .operations
                    .run_exclusive(crate::seal::run_seal_with_session(
                        agent, &session, &master, mode,
                    ))
                    .await?;
                Ok(serde_json::to_value(seal)?)
            }
            "StartCheckpoint" => {
                let checkpoint = crate::checkpoint::write_local_checkpoint(agent)?;
                if let Ok(session) = parse_session(&request.params) {
                    let master = master_from_params(&request.params, agent)?;
                    let (config, _session_guard) =
                        write_guarded_ephemeral_session(&agent.config.paths.run_root, &session)?;
                    let storage = RcloneStorage::from_session(&session, &config);
                    let _ = noland_storage::commit_checkpoint(&storage, &master, &checkpoint).await;
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
                let operation_id = uuid::Uuid::new_v4();
                let operation = OperationRecord {
                    operation_id,
                    kind: "restore".into(),
                    app_id: Some(app_id.clone()),
                    state: RestoreState::Queued.as_str().into(),
                    created_at: chrono::Utc::now(),
                    updated_at: chrono::Utc::now(),
                    last_error: None,
                    detail_json: json!({
                        "bundle_id": bundle_id,
                        "session_operation": session.operation_id,
                    }),
                };
                agent.db.upsert_operation(&operation)?;

                let task_agent = Arc::clone(agent);
                let terminated_agent = Arc::clone(agent);
                let operation_manager = agent.operations.clone();
                let started = Instant::now();
                let spawned = operation_manager.spawn(
                    operation_id,
                    async move {
                        if let Err(error) = crate::restore::run_restore_with_session(
                            &task_agent,
                            &app_id,
                            bundle_id,
                            mode,
                            &session,
                            &master,
                            operation_id,
                        )
                        .await
                        {
                            mark_operation_failed(
                                &task_agent,
                                operation_id,
                                error.to_string(),
                                started,
                            );
                        }
                    },
                    move |error| {
                        mark_operation_failed(&terminated_agent, operation_id, error, started);
                    },
                );
                if !spawned {
                    return Err(StateError::Invalid(format!(
                        "operation {operation_id} is already running"
                    )));
                }

                Ok(json!({
                    "operation_id": operation_id,
                    "state": RestoreState::Queued.as_str(),
                    "stage": RestoreState::Queued.as_str(),
                }))
            }
            "GetOperationStatus" | "GetBackupStatus" | "GetRestoreStatus" | "GetSealStatus" => {
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
                let mut value = serde_json::to_value(op)?;
                value
                    .as_object_mut()
                    .expect("operation serializes as an object")
                    .insert("running".into(), agent.operations.is_running(uuid).into());
                Ok(value)
            }
            "CancelBackup" | "CancelRestore" | "CancelOperation" => Err(StateError::Invalid(
                "operation cancellation is not implemented safely yet".into(),
            )),
            "ListLocalCommits" => {
                let app_id = AppId(req_str(&request.params, "app_id")?);
                Ok(json!(agent.db.latest_commit(&app_id)?))
            }
            "ListCloudCatalog" => {
                let session = parse_session(&request.params)?;
                let master = master_from_params(&request.params, agent)?;
                let (config, _session_guard) =
                    write_guarded_ephemeral_session(&agent.config.paths.run_root, &session)?;
                let storage = RcloneStorage::from_session(&session, &config);
                let catalog = load_catalog(&storage, &master).await;
                Ok(serde_json::to_value(catalog?)?)
            }
            "GetStorageHealth" => {
                if let Ok(session) = parse_session(&request.params) {
                    let (config, _session_guard) =
                        write_guarded_ephemeral_session(&agent.config.paths.run_root, &session)?;
                    let storage = RcloneStorage::from_session(&session, &config);
                    let health = storage.health_check().await?;
                    return Ok(serde_json::to_value(health)?);
                }
                Ok(json!({"ok": true, "provider": "none"}))
            }
            "ReconcileApp" => {
                let app_id = AppId(req_str(&request.params, "app_id")?);
                let n = crate::reconcile::reconcile_app(agent, &app_id)?;
                Ok(json!({"reconciled": n}))
            }
            "RefreshIndex" => {
                let processed_events = agent.process_events()?;
                let generation = agent.observer.loss_generation();
                agent.discover()?;

                let candidates = noland_discovery::filter_backup_candidates(agent.db.list_apps()?);
                let candidate_ids = candidates
                    .into_iter()
                    .map(|app| app.app_id)
                    .collect::<HashSet<_>>();
                let actions = reconciliation_actions(&candidate_ids, agent.db.list_dirty_apps()?);

                let mut apps_reconciled = 0_usize;
                let mut reconciled_paths = 0_usize;
                let mut excluded_flags_cleared = 0_usize;
                for action in actions {
                    match action {
                        ReconciliationAction::Reconcile(app_id) => {
                            reconciled_paths += crate::reconcile::reconcile_app(agent, &app_id)?;
                            apps_reconciled += 1;
                        }
                        ReconciliationAction::ClearExcluded(app_id) => {
                            agent.db.clear_reconciliation_required(&app_id)?;
                            excluded_flags_cleared += 1;
                        }
                    }
                }

                let loss_state_cleared = agent.observer.complete_reconciliation(generation);
                Ok(json!({
                    "apps_reconciled": apps_reconciled,
                    "paths_reconciled": reconciled_paths,
                    "excluded_flags_cleared": excluded_flags_cleared,
                    "processed_events": processed_events,
                    "loss_state_cleared": loss_state_cleared,
                }))
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

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u64::MAX as u128) as u64
}

fn record_discovery_duration(agent: &StateAgent, operation_id: uuid::Uuid, started: Instant) {
    let result = (|| -> Result<()> {
        let Some(mut op) = agent.db.get_operation(operation_id)? else {
            return Ok(());
        };
        let mut metrics = op
            .detail_json
            .get("metrics")
            .cloned()
            .and_then(|value| serde_json::from_value::<OperationMetrics>(value).ok())
            .unwrap_or_default();
        metrics.discovery_duration_ms = elapsed_ms(started);
        op.state = BackupState::Discovering.as_str().into();
        op.updated_at = chrono::Utc::now();
        if !op.detail_json.is_object() {
            op.detail_json = json!({});
        }
        op.detail_json
            .as_object_mut()
            .expect("operation detail is an object")
            .insert("metrics".into(), serde_json::to_value(metrics)?);
        agent.db.upsert_operation(&op)
    })();
    if let Err(error) = result {
        tracing::warn!(%operation_id, %error, "failed to persist discovery metrics");
    }
}

fn mark_all_backup_completed(
    agent: &StateAgent,
    operation_id: uuid::Uuid,
    count: usize,
    bundle_ids: &[uuid::Uuid],
    started: Instant,
) {
    let result = (|| -> Result<()> {
        let Some(mut op) = agent.db.get_operation(operation_id)? else {
            return Ok(());
        };
        let mut metrics = op
            .detail_json
            .get("metrics")
            .cloned()
            .and_then(|value| serde_json::from_value::<OperationMetrics>(value).ok())
            .unwrap_or_default();
        metrics.total_duration_ms = elapsed_ms(started);
        op.state = BackupState::Completed.as_str().into();
        op.updated_at = chrono::Utc::now();
        op.last_error = None;
        if !op.detail_json.is_object() {
            op.detail_json = json!({});
        }
        let detail = op
            .detail_json
            .as_object_mut()
            .expect("operation detail is an object");
        detail.insert("metrics".into(), serde_json::to_value(metrics)?);
        detail.insert("count".into(), count.into());
        detail.insert("bundle_ids".into(), serde_json::to_value(bundle_ids)?);
        agent.db.upsert_operation(&op)
    })();
    if let Err(error) = result {
        tracing::error!(%operation_id, %error, "failed to persist completed all-app backup");
    }
}

fn mark_operation_failed(
    agent: &StateAgent,
    operation_id: uuid::Uuid,
    error: String,
    started: Instant,
) {
    let result = (|| -> Result<()> {
        let Some(mut op) = agent.db.get_operation(operation_id)? else {
            return Ok(());
        };
        let mut metrics = op
            .detail_json
            .get("metrics")
            .cloned()
            .and_then(|value| serde_json::from_value::<OperationMetrics>(value).ok())
            .unwrap_or_default();
        metrics.total_duration_ms = elapsed_ms(started);
        op.state = BackupState::Failed.as_str().into();
        op.updated_at = chrono::Utc::now();
        op.last_error = Some(error);
        if !op.detail_json.is_object() {
            op.detail_json = json!({});
        }
        op.detail_json
            .as_object_mut()
            .expect("operation detail is an object")
            .insert("metrics".into(), serde_json::to_value(metrics)?);
        agent.db.upsert_operation(&op)
    })();
    if let Err(error) = result {
        tracing::error!(%operation_id, %error, "failed to persist failed backup");
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

#[derive(Debug, PartialEq, Eq)]
enum ReconciliationAction {
    Reconcile(AppId),
    ClearExcluded(AppId),
}

fn reconciliation_actions(
    candidate_ids: &HashSet<AppId>,
    dirty_apps: Vec<DirtyState>,
) -> Vec<ReconciliationAction> {
    dirty_apps
        .into_iter()
        .filter(|dirty| dirty.requires_reconciliation)
        .map(|dirty| {
            if candidate_ids.contains(&dirty.app_id) {
                ReconciliationAction::Reconcile(dirty.app_id)
            } else {
                ReconciliationAction::ClearExcluded(dirty.app_id)
            }
        })
        .collect()
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
    fn refresh_index_only_reconciles_flagged_candidates() {
        let candidate = AppId("app:candidate".into());
        let unflagged_candidate = AppId("app:unflagged".into());
        let excluded = AppId("exe:noland-state-agent".into());
        let candidate_ids = HashSet::from([candidate.clone(), unflagged_candidate.clone()]);
        let now = chrono::Utc::now();
        let dirty = |app_id, requires_reconciliation| DirtyState {
            app_id,
            first_dirty_at: now,
            last_dirty_at: now,
            dirty_paths: Vec::new(),
            requires_reconciliation,
        };

        let actions = reconciliation_actions(
            &candidate_ids,
            vec![
                dirty(candidate.clone(), true),
                dirty(unflagged_candidate, false),
                dirty(excluded.clone(), true),
            ],
        );

        assert_eq!(
            actions,
            vec![
                ReconciliationAction::Reconcile(candidate),
                ReconciliationAction::ClearExcluded(excluded),
            ]
        );
    }

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
