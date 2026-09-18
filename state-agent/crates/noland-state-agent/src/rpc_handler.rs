use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use chrono::Utc;
use noland_crypto::MasterKey;
use noland_rclone_adapter::EphemeralRcloneSession;
use noland_rpc::{
    ActiveAppSession, GetActiveAppSessionsResult, HealthStatus, ResolvedProcessApp, RpcHandler,
    RpcRequest, VerifyBackupCommitResult,
};
use noland_state_core::*;
use noland_storage::{
    load_catalog, write_guarded_ephemeral_session, RcloneStorage, SharedStorageProvider,
};
use serde_json::json;
use uuid::Uuid;

use crate::operation_manager::{CancelOutcome, OperationLane};
use crate::StateAgent;

const AGENT_API_VERSION: u64 = 13;
const DEFAULT_RECENT_OPERATION_LIMIT: usize = 50;
const MAX_DIAGNOSTIC_OPERATION_LIMIT: usize = 1_000;

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
            "GetActiveAppSessions" => {
                reconcile_process_state_best_effort(agent);
                Ok(serde_json::to_value(active_app_sessions(&agent.db)?)?)
            }
            "ResolveProcessToApp" => {
                let pid = req_pid(&request.params)?;
                Ok(serde_json::to_value(resolve_process_to_app(
                    &agent.db, pid,
                )?)?)
            }
            "VerifyBackupCommit" => {
                let app_id = AppId(req_str(&request.params, "app_id")?);
                let bundle_id = req_uuid(&request.params, "bundle_id")?;
                let commit_id = req_uuid(&request.params, "commit_id")?;
                Ok(serde_json::to_value(verify_backup_commit(
                    &agent.db, app_id, bundle_id, commit_id,
                )?)?)
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
                let performance = BackupPerformanceMode::parse(
                    request
                        .params
                        .get("performance_mode")
                        .and_then(|value| value.as_str())
                        .unwrap_or("balanced"),
                );
                let session = parse_session(&request.params)?;
                let master = master_from_params(&request.params, agent)?;
                let retry_operation_id = internal_retry_operation_id(&request.params)?;
                let op_id = retry_operation_id.unwrap_or_else(uuid::Uuid::new_v4);
                let app_id = if app_id_raw == "*" || app_id_raw == "_all" {
                    None
                } else {
                    Some(AppId(app_id_raw.into()))
                };
                upsert_queued_operation(
                    agent,
                    op_id,
                    "backup",
                    app_id.clone(),
                    BackupState::Queued.as_str(),
                    json!({
                        "app_id": app_id_raw,
                        "mode": mode.as_str(),
                        "performance_mode": performance.as_str(),
                    }),
                    retry_operation_id.is_some(),
                )?;
                remember_retry_request(agent, op_id, "StartBackup", &request.params);

                let all_apps = app_id.is_none();
                let task_agent = Arc::clone(agent);
                let cancelled_agent = Arc::clone(agent);
                let terminated_agent = Arc::clone(agent);
                let operation_manager = agent.operations.clone();
                let total_started = Instant::now();
                let spawned = operation_manager.spawn_cancellable_on_lane(
                    op_id,
                    OperationLane::Transfer,
                    async move {
                        let discovery_started = Instant::now();
                        let discovery = task_agent
                            .discover()
                            .and_then(|_| task_agent.process_events().map(|_| ()));
                        if let Err(error) = discovery {
                            if task_agent.operations.cancel_requested(op_id) {
                                mark_operation_cancelled(&task_agent, op_id);
                            } else {
                                mark_operation_failed(
                                    &task_agent,
                                    op_id,
                                    error.to_string(),
                                    total_started,
                                );
                            }
                            return;
                        }
                        record_discovery_duration(&task_agent, op_id, discovery_started);

                        let result = if all_apps {
                            crate::backup::run_backup_all_with_session_performance(
                                &task_agent,
                                mode,
                                performance,
                                &session,
                                &master,
                                Some(op_id),
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
                            crate::backup::run_backup_with_session_performance(
                                &task_agent,
                                app_id.as_ref().expect("single-app backup has an app id"),
                                mode,
                                performance,
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
                            Err(_) if task_agent.operations.cancel_requested(op_id) => {
                                mark_operation_cancelled(&task_agent, op_id);
                            }
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
                    move || mark_operation_cancelled(&cancelled_agent, op_id),
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
                let retry_operation_id = internal_retry_operation_id(&request.params)?;
                let operation_id = retry_operation_id.unwrap_or_else(uuid::Uuid::new_v4);
                upsert_queued_operation(
                    agent,
                    operation_id,
                    "seal",
                    None,
                    SealState::Requested.as_str(),
                    json!({"session_operation": session.operation_id}),
                    retry_operation_id.is_some(),
                )?;
                set_operation_phase(agent, operation_id, "queued", "Seal queued")?;
                remember_retry_request(agent, operation_id, "StartSeal", &request.params);

                let task_agent = Arc::clone(agent);
                let cancelled_agent = Arc::clone(agent);
                let terminated_agent = Arc::clone(agent);
                let started = Instant::now();
                let spawned = agent.operations.spawn_cancellable(
                    operation_id,
                    async move {
                        set_operation_phase_best_effort(
                            &task_agent,
                            operation_id,
                            "sealing",
                            "Seal in progress",
                        );
                        match crate::seal::run_seal_with_session(
                            &task_agent,
                            &session,
                            &master,
                            mode,
                        )
                        .await
                        {
                            Ok(seal) => mark_operation_completed(
                                &task_agent,
                                operation_id,
                                SealState::Sealed.as_str(),
                                json!({
                                    "seal_id": seal.seal_id,
                                    "checkpoint_id": seal.checkpoint_id,
                                    "seal": seal,
                                }),
                                started,
                            ),
                            Err(error) => mark_operation_failed(
                                &task_agent,
                                operation_id,
                                error.to_string(),
                                started,
                            ),
                        }
                    },
                    move || mark_operation_cancelled(&cancelled_agent, operation_id),
                    move |error| {
                        mark_operation_failed(&terminated_agent, operation_id, error, started)
                    },
                );
                if !spawned {
                    return Err(StateError::Invalid(format!(
                        "operation {operation_id} is already running"
                    )));
                }
                Ok(queued_response(operation_id, SealState::Requested.as_str()))
            }
            "StartCheckpoint" => {
                let session = optional_session(&request.params)?;
                let master = session
                    .as_ref()
                    .map(|_| master_from_params(&request.params, agent))
                    .transpose()?;
                let retry_operation_id = internal_retry_operation_id(&request.params)?;
                let operation_id = retry_operation_id.unwrap_or_else(uuid::Uuid::new_v4);
                upsert_queued_operation(
                    agent,
                    operation_id,
                    "checkpoint",
                    None,
                    BackupState::Queued.as_str(),
                    json!({
                        "session_operation": session.as_ref().map(|value| value.operation_id.as_str()),
                        "remote_commit_requested": session.is_some(),
                    }),
                    retry_operation_id.is_some(),
                )?;
                set_operation_phase(agent, operation_id, "queued", "Checkpoint queued")?;
                remember_retry_request(agent, operation_id, "StartCheckpoint", &request.params);

                let task_agent = Arc::clone(agent);
                let cancelled_agent = Arc::clone(agent);
                let terminated_agent = Arc::clone(agent);
                let started = Instant::now();
                let spawned = agent.operations.spawn_cancellable(
                    operation_id,
                    async move {
                        set_operation_phase_best_effort(
                            &task_agent,
                            operation_id,
                            "checkpointing",
                            "Writing local checkpoint",
                        );
                        let result = async {
                            let checkpoint =
                                crate::checkpoint::write_local_checkpoint(&task_agent)?;
                            if let (Some(session), Some(master)) =
                                (session.as_ref(), master.as_ref())
                            {
                                set_operation_phase_best_effort(
                                    &task_agent,
                                    operation_id,
                                    "uploading",
                                    "Committing checkpoint to shared storage",
                                );
                                let (config, _session_guard) = write_guarded_ephemeral_session(
                                    &task_agent.config.paths.run_root,
                                    session,
                                )?;
                                let storage = RcloneStorage::try_from_session(session, &config)?;
                                noland_storage::commit_checkpoint(&storage, master, &checkpoint)
                                    .await?;
                            }
                            Result::<_>::Ok(checkpoint)
                        }
                        .await;
                        match result {
                            Ok(checkpoint) => mark_operation_completed(
                                &task_agent,
                                operation_id,
                                BackupState::Completed.as_str(),
                                json!({"checkpoint_id": checkpoint.checkpoint_id}),
                                started,
                            ),
                            Err(error) => mark_operation_failed(
                                &task_agent,
                                operation_id,
                                error.to_string(),
                                started,
                            ),
                        }
                    },
                    move || mark_operation_cancelled(&cancelled_agent, operation_id),
                    move |error| {
                        mark_operation_failed(&terminated_agent, operation_id, error, started)
                    },
                );
                if !spawned {
                    return Err(StateError::Invalid(format!(
                        "operation {operation_id} is already running"
                    )));
                }
                Ok(queued_response(operation_id, BackupState::Queued.as_str()))
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
                let retry_operation_id = internal_retry_operation_id(&request.params)?;
                let operation_id = retry_operation_id.unwrap_or_else(uuid::Uuid::new_v4);
                upsert_queued_operation(
                    agent,
                    operation_id,
                    "restore",
                    Some(app_id.clone()),
                    RestoreState::Queued.as_str(),
                    json!({
                        "app_id": app_id.0.clone(),
                        "mode": match mode {
                            RestoreMode::CompleteApplication => "complete_application",
                            RestoreMode::Custom => "custom",
                            RestoreMode::PersonalState => "personal_state",
                        },
                        "bundle_id": bundle_id,
                    }),
                    retry_operation_id.is_some(),
                )?;
                remember_retry_request(agent, operation_id, "StartRestore", &request.params);

                let task_agent = Arc::clone(agent);
                let cancelled_agent = Arc::clone(agent);
                let terminated_agent = Arc::clone(agent);
                let operation_manager = agent.operations.clone();
                let started = Instant::now();
                let spawned = operation_manager.spawn_cancellable_on_lane(
                    operation_id,
                    OperationLane::Transfer,
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
                    move || mark_operation_cancelled(&cancelled_agent, operation_id),
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
                let operation_id = operation_id_from_params(&request.params)?;
                let operation = agent
                    .db
                    .get_operation(operation_id)?
                    .ok_or_else(|| StateError::NotFound(operation_id.to_string()))?;
                let include_journal = request
                    .params
                    .get("include_journal")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);
                serialize_operation(agent, operation, include_journal)
            }
            "CancelBackup" | "CancelRestore" | "CancelOperation" => {
                let operation_id = operation_id_from_params(&request.params)?;
                let operation = agent
                    .db
                    .get_operation(operation_id)?
                    .ok_or_else(|| StateError::NotFound(operation_id.to_string()))?;
                if is_terminal_state(&operation.state) {
                    return Err(StateError::Invalid(format!(
                        "operation {operation_id} is already terminal ({})",
                        operation.state
                    )));
                }
                match agent.operations.cancel(operation_id) {
                    CancelOutcome::Requested => Ok(json!({
                        "operation_id": operation_id,
                        "accepted": true,
                        "cancel_requested": true,
                        "cancelled": false,
                        "state": "CANCEL_REQUESTED",
                    })),
                    CancelOutcome::AlreadyRequested => Ok(json!({
                        "operation_id": operation_id,
                        "accepted": true,
                        "cancel_requested": true,
                        "cancelled": false,
                        "state": "CANCEL_REQUESTED",
                        "already_requested": true,
                    })),
                    CancelOutcome::NotRunning => Err(StateError::Invalid(format!(
                        "operation {operation_id} is not running; cancellation was not accepted"
                    ))),
                }
            }
            "RetryOperation" => {
                let operation_id = operation_id_from_params(&request.params)?;
                let operation = agent
                    .db
                    .get_operation(operation_id)?
                    .ok_or_else(|| StateError::NotFound(operation_id.to_string()))?;
                if agent.operations.is_running(operation_id) {
                    return Err(StateError::Invalid(format!(
                        "operation {operation_id} is still running"
                    )));
                }
                if !is_retryable_state(&operation.state) {
                    return Err(StateError::Invalid(format!(
                        "operation {operation_id} cannot be retried from state {}",
                        operation.state
                    )));
                }
                let descriptor = retry_descriptor_for(agent, &operation)?;
                let mut params = descriptor.params;
                overlay_retry_secrets(&mut params, &request.params)?;
                let object = params.as_object_mut().ok_or_else(|| {
                    StateError::Invalid("stored retry parameters are not an object".into())
                })?;
                if !object.contains_key("session") {
                    return Err(StateError::Invalid(format!(
                        "retry for operation {operation_id} needs a new session; resubmit the original request"
                    )));
                }
                object.insert(
                    "_retry_operation_id".into(),
                    serde_json::Value::String(operation_id.to_string()),
                );
                let reset_journal_entries = reset_retryable_journal_entries(agent, operation_id)?;
                let retry_request = RpcRequest {
                    id: request.id.clone(),
                    method: descriptor.method,
                    params,
                };
                let mut response = Box::pin(self.handle(&retry_request)).await?;
                if let Some(fields) = response.as_object_mut() {
                    fields.insert("retried".into(), true.into());
                    fields.insert("retried_operation_id".into(), json!(operation_id));
                    fields.insert("reset_journal_entries".into(), reset_journal_entries.into());
                }
                Ok(response)
            }
            "ListRecentOperations" => {
                let limit = requested_limit(&request.params, DEFAULT_RECENT_OPERATION_LIMIT);
                let app_id = request
                    .params
                    .get("app_id")
                    .and_then(|value| value.as_str())
                    .map(|value| AppId(value.to_string()));
                let operations = if let Some(app_id) = app_id.as_ref() {
                    agent.db.recent_operations_for_app(app_id, limit)?
                } else {
                    agent.db.recent_operations(limit)?
                };
                let include_journal = request
                    .params
                    .get("include_journal")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);
                let operations = operations
                    .into_iter()
                    .map(|operation| serialize_operation(agent, operation, include_journal))
                    .collect::<Result<Vec<_>>>()?;
                Ok(json!({
                    "operations": operations,
                    "limit": limit,
                }))
            }
            "GetPerformanceDiagnostics" => performance_diagnostics(agent, &request.params),
            "GetDirtyState" => dirty_state(agent, &request.params),
            "GetIncrementalIndexStats" => incremental_index_stats(agent, &request.params),
            "ListLocalCommits" => {
                let app_id = AppId(req_str(&request.params, "app_id")?);
                Ok(json!(agent.db.latest_commit(&app_id)?))
            }
            "ListCloudCatalog" => {
                let session = parse_session(&request.params)?;
                let master = master_from_params(&request.params, agent)?;
                let (config, _session_guard) =
                    write_guarded_ephemeral_session(&agent.config.paths.run_root, &session)?;
                let storage = RcloneStorage::try_from_session(&session, &config)?;
                let catalog = load_catalog(&storage, &master).await;
                Ok(serde_json::to_value(catalog?)?)
            }
            "GetStorageHealth" => {
                if let Ok(session) = parse_session(&request.params) {
                    let (config, _session_guard) =
                        write_guarded_ephemeral_session(&agent.config.paths.run_root, &session)?;
                    let storage = RcloneStorage::try_from_session(&session, &config)?;
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
                let generation = agent.observer.loss_generation();
                agent.discover()?;
                let live_process_recovery = agent.reconcile_live_processes()?;
                let processed_events = agent.process_events()?;

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
                    "live_process_recovery": live_process_recovery,
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

fn operation_id_from_params(params: &serde_json::Value) -> Result<uuid::Uuid> {
    let raw = req_str(params, "operation_id")?;
    uuid::Uuid::parse_str(&raw).map_err(|error| StateError::Invalid(error.to_string()))
}

fn internal_retry_operation_id(params: &serde_json::Value) -> Result<Option<uuid::Uuid>> {
    params
        .get("_retry_operation_id")
        .and_then(|value| value.as_str())
        .map(|raw| {
            uuid::Uuid::parse_str(raw).map_err(|error| StateError::Invalid(error.to_string()))
        })
        .transpose()
}

fn optional_session(params: &serde_json::Value) -> Result<Option<EphemeralRcloneSession>> {
    match params.get("session") {
        Some(value) => serde_json::from_value(value.clone())
            .map(Some)
            .map_err(|error| StateError::Invalid(error.to_string())),
        None => Ok(None),
    }
}

fn requested_limit(params: &serde_json::Value, default: usize) -> usize {
    params
        .get("limit")
        .and_then(|value| value.as_u64())
        .map(|value| value.min(MAX_DIAGNOSTIC_OPERATION_LIMIT as u64) as usize)
        .unwrap_or(default)
}

fn queued_response(operation_id: uuid::Uuid, state: &str) -> serde_json::Value {
    json!({
        "operation_id": operation_id,
        "state": state,
        "stage": state,
    })
}

fn remember_retry_request(
    agent: &StateAgent,
    operation_id: uuid::Uuid,
    method: &str,
    params: &serde_json::Value,
) {
    let mut in_memory = params.clone();
    if let Some(fields) = in_memory.as_object_mut() {
        fields.remove("_retry_operation_id");
    }
    agent
        .operations
        .remember_retry(operation_id, method, &in_memory);
    let sanitized = sanitize_retry_params(params);
    if let Ok(Some(mut operation)) = agent.db.get_operation(operation_id) {
        if !operation.detail_json.is_object() {
            operation.detail_json = json!({});
        }
        if let Some(fields) = operation.detail_json.as_object_mut() {
            fields.insert(
                "retry".into(),
                json!({
                    "method": method,
                    "params": sanitized,
                }),
            );
        }
        if let Err(error) = agent.db.upsert_operation(&operation) {
            tracing::warn!(%operation_id, %error, "failed to persist retry context");
        }
    }
}

fn sanitize_retry_params(params: &serde_json::Value) -> serde_json::Value {
    let mut sanitized = params.clone();
    if let Some(fields) = sanitized.as_object_mut() {
        fields.remove("_retry_operation_id");
        fields.remove("session");
        fields.remove("master_key_hex");
    }
    sanitized
}

fn overlay_retry_secrets(
    stored: &mut serde_json::Value,
    incoming: &serde_json::Value,
) -> Result<()> {
    let Some(fields) = stored.as_object_mut() else {
        return Err(StateError::Invalid(
            "stored retry parameters are not an object".into(),
        ));
    };
    if let Some(session) = incoming.get("session") {
        fields.insert("session".into(), session.clone());
    }
    if let Some(master_key) = incoming.get("master_key_hex") {
        fields.insert("master_key_hex".into(), master_key.clone());
    }
    Ok(())
}

fn retry_descriptor_for(
    agent: &StateAgent,
    operation: &OperationRecord,
) -> Result<crate::operation_manager::RetryDescriptor> {
    let detail = &operation.detail_json;
    let (method, params) = match operation.kind.as_str() {
        "backup" => {
            let app_id = detail
                .get("app_id")
                .and_then(|value| value.as_str())
                .or_else(|| operation.app_id.as_ref().map(|app_id| app_id.0.as_str()))
                .unwrap_or("*");
            (
                "StartBackup",
                json!({
                    "app_id": app_id,
                    "mode": detail.get("mode").and_then(|value| value.as_str()).unwrap_or("personal_state"),
                    "performance_mode": detail.get("performance_mode").and_then(|value| value.as_str()).unwrap_or("balanced"),
                }),
            )
        }
        "restore" => {
            let app_id = detail
                .get("app_id")
                .and_then(|value| value.as_str())
                .or_else(|| operation.app_id.as_ref().map(|app_id| app_id.0.as_str()))
                .ok_or_else(|| StateError::Invalid("persisted restore app_id is missing".into()))?;
            let bundle_id = detail
                .get("bundle_id")
                .and_then(|value| value.as_str())
                .ok_or_else(|| {
                    StateError::Invalid("persisted restore bundle_id is missing".into())
                })?;
            (
                "StartRestore",
                json!({
                    "app_id": app_id,
                    "mode": detail.get("mode").and_then(|value| value.as_str()).unwrap_or("personal_state"),
                    "bundle_id": bundle_id,
                }),
            )
        }
        kind => {
            return Err(StateError::Invalid(format!(
                "operation {} of kind {kind} cannot be reconstructed for retry",
                operation.operation_id
            )))
        }
    };
    agent
        .operations
        .remember_retry(operation.operation_id, method, &params);
    agent
        .operations
        .retry_descriptor(operation.operation_id)
        .ok_or_else(|| {
            StateError::Invalid(format!(
                "retry context for operation {} is unavailable",
                operation.operation_id
            ))
        })
}

fn upsert_queued_operation(
    agent: &StateAgent,
    operation_id: uuid::Uuid,
    kind: &str,
    app_id: Option<AppId>,
    state: &str,
    detail: serde_json::Value,
    retry: bool,
) -> Result<()> {
    let now = chrono::Utc::now();
    let mut operation = if retry {
        let existing = agent
            .db
            .get_operation(operation_id)?
            .ok_or_else(|| StateError::NotFound(operation_id.to_string()))?;
        if existing.kind != kind {
            return Err(StateError::Invalid(format!(
                "operation {operation_id} is {}, not {kind}",
                existing.kind
            )));
        }
        existing
    } else {
        OperationRecord {
            operation_id,
            kind: kind.to_string(),
            app_id: app_id.clone(),
            state: state.to_string(),
            created_at: now,
            updated_at: now,
            last_error: None,
            detail_json: json!({}),
        }
    };
    operation.app_id = app_id;
    operation.state = state.to_string();
    operation.updated_at = now;
    operation.last_error = None;
    if !operation.detail_json.is_object() {
        operation.detail_json = json!({});
    }
    let fields = operation
        .detail_json
        .as_object_mut()
        .expect("operation detail is an object");
    if let Some(new_fields) = detail.as_object() {
        for (key, value) in new_fields {
            fields.insert(key.clone(), value.clone());
        }
    }
    if retry {
        let retry_count = fields
            .get("retry_count")
            .and_then(|value| value.as_u64())
            .unwrap_or(0)
            .saturating_add(1);
        fields.insert("retry_count".into(), retry_count.into());
        fields.insert("retried_at".into(), json!(now));
    }
    agent.db.upsert_operation(&operation)?;
    set_operation_phase(
        agent,
        operation_id,
        "queued",
        if retry {
            "Operation retry queued"
        } else {
            "Operation queued"
        },
    )
}

fn set_operation_phase(
    agent: &StateAgent,
    operation_id: uuid::Uuid,
    phase: &str,
    message: &str,
) -> Result<()> {
    let mut progress = OperationProgress::new(phase, 0);
    progress.message = Some(message.to_string());
    agent
        .db
        .set_operation_progress(operation_id, Some(&progress))
}

fn set_operation_phase_best_effort(
    agent: &StateAgent,
    operation_id: uuid::Uuid,
    phase: &str,
    message: &str,
) {
    if let Err(error) = set_operation_phase(agent, operation_id, phase, message) {
        tracing::warn!(%operation_id, %error, "failed to persist operation progress");
    }
}

fn serialize_operation(
    agent: &StateAgent,
    operation: OperationRecord,
    include_journal: bool,
) -> Result<serde_json::Value> {
    let operation_id = operation.operation_id;
    let detail_metrics = operation
        .detail_json
        .get("metrics")
        .cloned()
        .and_then(|value| serde_json::from_value::<OperationMetrics>(value).ok());
    let metrics = agent
        .db
        .get_operation_metrics(operation_id)?
        .or(detail_metrics);
    let progress = agent.db.get_operation_progress(operation_id)?;
    let journal_summary = agent.db.sync_journal_summary(operation_id)?;
    let pack_transfer = agent
        .db
        .sync_journal_summary_for_kind(operation_id, ContentObjectKind::Pack)?;
    let mut value = serde_json::to_value(operation)?;
    let fields = value
        .as_object_mut()
        .expect("operation serializes as an object");
    fields.insert(
        "running".into(),
        agent.operations.is_running(operation_id).into(),
    );
    fields.insert(
        "cancel_requested".into(),
        agent.operations.cancel_requested(operation_id).into(),
    );
    fields.insert("progress".into(), serde_json::to_value(progress)?);
    fields.insert("metrics".into(), serde_json::to_value(metrics)?);
    fields.insert(
        "sync_journal".into(),
        serde_json::to_value(journal_summary)?,
    );
    fields.insert("pack_transfer".into(), serde_json::to_value(pack_transfer)?);
    if include_journal {
        fields.insert(
            "sync_journal_entries".into(),
            serde_json::to_value(agent.db.list_sync_journal_entries(operation_id, None)?)?,
        );
    }
    Ok(value)
}

fn reset_retryable_journal_entries(agent: &StateAgent, operation_id: uuid::Uuid) -> Result<usize> {
    let mut reset = 0;
    for entry in agent.db.list_sync_journal_entries(operation_id, None)? {
        if matches!(
            entry.state,
            SyncJournalState::InProgress
                | SyncJournalState::RetryScheduled
                | SyncJournalState::Failed
        ) {
            if agent.db.set_sync_journal_state(
                operation_id,
                &entry.item_key,
                SyncJournalState::Pending,
                None,
                None,
            )? {
                reset += 1;
            }
        }
    }
    Ok(reset)
}

fn is_terminal_state(state: &str) -> bool {
    matches!(
        state,
        "COMPLETED" | "FAILED" | "CANCELLED" | "ROLLED_BACK" | "SEALED" | "INTERRUPTED"
    )
}

fn is_retryable_state(state: &str) -> bool {
    matches!(
        state,
        "FAILED" | "CANCELLED" | "ROLLED_BACK" | "INTERRUPTED"
    )
}

fn operation_metrics(agent: &StateAgent, operation: &OperationRecord) -> Result<OperationMetrics> {
    Ok(agent
        .db
        .get_operation_metrics(operation.operation_id)?
        .or_else(|| {
            operation
                .detail_json
                .get("metrics")
                .cloned()
                .and_then(|value| serde_json::from_value(value).ok())
        })
        .unwrap_or_default())
}

fn performance_diagnostics(
    agent: &StateAgent,
    params: &serde_json::Value,
) -> Result<serde_json::Value> {
    let limit = requested_limit(params, DEFAULT_RECENT_OPERATION_LIMIT);
    let operations = agent.db.recent_operations(limit)?;
    let mut completed = 0_u64;
    let mut failed = 0_u64;
    let mut cancelled = 0_u64;
    let mut total_duration_ms = 0_u64;
    let mut bytes_hashed = 0_u64;
    let mut bytes_uploaded = 0_u64;
    let mut bytes_downloaded = 0_u64;
    let mut files_rehashed = 0_u64;
    let mut files_skipped_fast_identity = 0_u64;
    let mut planning_ms = 0_u64;
    let mut hashing_ms = 0_u64;
    let mut packing_ms = 0_u64;
    let mut upload_ms = 0_u64;
    let mut download_ms = 0_u64;
    let mut operation_values = Vec::with_capacity(operations.len());
    for operation in operations {
        match operation.state.as_str() {
            "COMPLETED" | "SEALED" => completed += 1,
            "FAILED" | "ROLLED_BACK" => failed += 1,
            "CANCELLED" => cancelled += 1,
            _ => {}
        }
        let metrics = operation_metrics(agent, &operation)?;
        total_duration_ms = total_duration_ms.saturating_add(metrics.total_duration_ms);
        bytes_hashed = bytes_hashed.saturating_add(metrics.bytes_hashed);
        bytes_uploaded = bytes_uploaded.saturating_add(metrics.bytes_uploaded);
        bytes_downloaded = bytes_downloaded.saturating_add(metrics.bytes_downloaded);
        files_rehashed = files_rehashed.saturating_add(metrics.num_files_rehashed);
        files_skipped_fast_identity =
            files_skipped_fast_identity.saturating_add(metrics.num_files_skipped_fast_identity);
        planning_ms = planning_ms.saturating_add(metrics.planning_duration_ms);
        hashing_ms = hashing_ms.saturating_add(metrics.hashing_duration_ms);
        packing_ms = packing_ms.saturating_add(metrics.packing_duration_ms);
        upload_ms = upload_ms.saturating_add(metrics.upload_duration_ms);
        download_ms = download_ms.saturating_add(metrics.download_duration_ms);
        operation_values.push(serialize_operation(agent, operation, false)?);
    }
    let sample_size = operation_values.len() as u64;
    let average_total_duration_ms = (sample_size > 0).then(|| total_duration_ms / sample_size);
    let dominant_stage = [
        ("planning", planning_ms),
        ("hashing", hashing_ms),
        ("packing", packing_ms),
        ("upload", upload_ms),
        ("download", download_ms),
    ]
    .into_iter()
    .max_by_key(|(_, duration)| *duration)
    .map(|(stage, _)| stage);
    let upload_bytes_per_second =
        (upload_ms > 0).then(|| bytes_uploaded.saturating_mul(1_000) / upload_ms.max(1));
    let download_bytes_per_second =
        (download_ms > 0).then(|| bytes_downloaded.saturating_mul(1_000) / download_ms.max(1));
    let remote_transfer_is_dominant = upload_ms.saturating_add(download_ms)
        > planning_ms
            .saturating_add(hashing_ms)
            .saturating_add(packing_ms);
    let running = agent
        .operations
        .running_snapshots()
        .into_iter()
        .map(|operation| {
            json!({
                "operation_id": operation.operation_id,
                "cancel_requested": operation.cancel_requested,
                "queued_for_ms": operation.queued_for_ms,
                "running_for_ms": operation.running_for_ms,
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "captured_at": chrono::Utc::now(),
        "process_metrics": agent.metrics.snapshot(),
        "observer": agent.observer.status(agent.hub.queue.dropped()),
        "operation_manager": {
            "running_count": running.len(),
            "running": running,
        },
        "summary": {
            "sample_size": sample_size,
            "completed": completed,
            "failed": failed,
            "cancelled": cancelled,
            "total_duration_ms": total_duration_ms,
            "average_total_duration_ms": average_total_duration_ms,
            "bytes_hashed": bytes_hashed,
            "bytes_uploaded": bytes_uploaded,
            "bytes_downloaded": bytes_downloaded,
            "files_rehashed": files_rehashed,
            "files_skipped_fast_identity": files_skipped_fast_identity,
            "stage_duration_ms": {
                "planning": planning_ms,
                "hashing": hashing_ms,
                "packing": packing_ms,
                "upload": upload_ms,
                "download": download_ms,
            },
            "dominant_stage": dominant_stage,
            "remote_transfer_is_dominant": remote_transfer_is_dominant,
            "upload_bytes_per_second": upload_bytes_per_second,
            "download_bytes_per_second": download_bytes_per_second,
        },
        "recent_operations": operation_values,
    }))
}

fn dirty_state(agent: &StateAgent, params: &serde_json::Value) -> Result<serde_json::Value> {
    let app_id = params
        .get("app_id")
        .and_then(|value| value.as_str())
        .map(|value| AppId(value.to_string()));
    let mut apps = agent.db.list_dirty_apps()?;
    if let Some(app_id) = app_id.as_ref() {
        apps.retain(|dirty| &dirty.app_id == app_id);
    }
    let roots = agent.db.list_dirty_roots(app_id.as_ref())?;
    let dirty_paths = apps
        .iter()
        .map(|dirty| dirty.dirty_paths.len() as u64)
        .sum::<u64>();
    let mutation_count = roots.iter().map(|root| root.mutation_count).sum::<u64>();
    let reconciliation_required_apps = apps
        .iter()
        .filter(|dirty| dirty.requires_reconciliation)
        .count();
    let reconciliation_required_roots = roots
        .iter()
        .filter(|root| root.requires_reconciliation)
        .count();
    Ok(json!({
        "captured_at": chrono::Utc::now(),
        "summary": {
            "dirty_apps": apps.len(),
            "dirty_paths": dirty_paths,
            "dirty_roots": roots.len(),
            "root_mutations": mutation_count,
            "reconciliation_required_apps": reconciliation_required_apps,
            "reconciliation_required_roots": reconciliation_required_roots,
        },
        "apps": apps,
        "roots": roots,
    }))
}

#[derive(Default)]
struct FileIndexStats {
    files: u64,
    bytes: u64,
    hashed: u64,
    trusted: u64,
    verify_required: u64,
    dirty: u64,
    missing: u64,
}

impl FileIndexStats {
    fn add(&mut self, state: &FileStateRecord) {
        self.files = self.files.saturating_add(1);
        self.bytes = self.bytes.saturating_add(state.size);
        self.hashed = self
            .hashed
            .saturating_add(u64::from(state.content_hash.is_some()));
        match state.trust {
            FileStateTrust::Trusted => self.trusted += 1,
            FileStateTrust::VerifyRequired => self.verify_required += 1,
            FileStateTrust::Dirty => self.dirty += 1,
            FileStateTrust::Missing => self.missing += 1,
        }
    }

    fn value(&self) -> serde_json::Value {
        json!({
            "files": self.files,
            "bytes": self.bytes,
            "hashed_files": self.hashed,
            "trusted_files": self.trusted,
            "verify_required_files": self.verify_required,
            "dirty_files": self.dirty,
            "missing_files": self.missing,
        })
    }
}

fn incremental_index_stats(
    agent: &StateAgent,
    params: &serde_json::Value,
) -> Result<serde_json::Value> {
    let requested_app = params
        .get("app_id")
        .and_then(|value| value.as_str())
        .map(|value| AppId(value.to_string()));
    let apps = if let Some(app_id) = requested_app.as_ref() {
        vec![app_id.clone()]
    } else {
        agent
            .db
            .list_apps()?
            .into_iter()
            .map(|app| app.app_id)
            .collect()
    };
    let mut total = FileIndexStats::default();
    let mut indexed_apps = 0_u64;
    let mut pending_mutations = 0_u64;
    let mut by_app = Vec::with_capacity(apps.len());
    for app_id in apps {
        let states = agent.db.list_file_states(&app_id, None)?;
        let mut stats = FileIndexStats::default();
        for state in &states {
            stats.add(state);
            total.add(state);
        }
        if !states.is_empty() {
            indexed_apps += 1;
        }
        let app_pending_mutations = agent.db.count_pending_app_mutations(&app_id)?;
        pending_mutations = pending_mutations.saturating_add(app_pending_mutations);
        by_app.push(json!({
            "app_id": app_id,
            "pending_mutations": app_pending_mutations,
            "index": stats.value(),
        }));
    }
    let dirty_roots = agent.db.list_dirty_roots(requested_app.as_ref())?;
    Ok(json!({
        "captured_at": chrono::Utc::now(),
        "summary": {
            "apps_considered": by_app.len(),
            "indexed_apps": indexed_apps,
            "pending_mutations": pending_mutations,
            "dirty_roots": dirty_roots.len(),
            "index": total.value(),
        },
        "by_app": by_app,
    }))
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
        detail.insert("metrics".into(), serde_json::to_value(&metrics)?);
        detail.insert("count".into(), count.into());
        detail.insert("bundle_ids".into(), serde_json::to_value(bundle_ids)?);
        agent.db.set_operation_metrics(operation_id, &metrics)?;
        agent.db.set_operation_progress(operation_id, None)?;
        agent.db.upsert_operation(&op)
    })();
    if let Err(error) = result {
        tracing::error!(%operation_id, %error, "failed to persist completed all-app backup");
    }
}

fn mark_operation_completed(
    agent: &StateAgent,
    operation_id: uuid::Uuid,
    state: &str,
    detail: serde_json::Value,
    started: Instant,
) {
    let result = (|| -> Result<()> {
        let Some(mut operation) = agent.db.get_operation(operation_id)? else {
            return Ok(());
        };
        let mut metrics = operation_metrics(agent, &operation)?;
        metrics.total_duration_ms = elapsed_ms(started);
        operation.state = state.to_string();
        operation.updated_at = chrono::Utc::now();
        operation.last_error = None;
        if !operation.detail_json.is_object() {
            operation.detail_json = json!({});
        }
        let fields = operation
            .detail_json
            .as_object_mut()
            .expect("operation detail is an object");
        if let Some(completed_fields) = detail.as_object() {
            for (key, value) in completed_fields {
                fields.insert(key.clone(), value.clone());
            }
        }
        fields.insert("metrics".into(), serde_json::to_value(&metrics)?);
        agent.db.set_operation_metrics(operation_id, &metrics)?;
        agent.db.set_operation_progress(operation_id, None)?;
        agent.db.upsert_operation(&operation)
    })();
    if let Err(error) = result {
        tracing::error!(%operation_id, %error, "failed to persist completed operation");
    }
}

fn mark_operation_cancelled(agent: &StateAgent, operation_id: uuid::Uuid) {
    let result = (|| -> Result<()> {
        let Some(mut operation) = agent.db.get_operation(operation_id)? else {
            return Ok(());
        };
        if is_terminal_state(&operation.state) {
            return Ok(());
        }
        let now = chrono::Utc::now();
        operation.state = BackupState::Cancelled.as_str().into();
        operation.updated_at = now;
        operation.last_error = None;
        if !operation.detail_json.is_object() {
            operation.detail_json = json!({});
        }
        let fields = operation
            .detail_json
            .as_object_mut()
            .expect("operation detail is an object");
        fields.insert("cancelled_at".into(), json!(now));
        fields.insert("cooperative_cancellation".into(), true.into());
        agent.db.upsert_operation(&operation)?;

        let mut progress = OperationProgress::new("cancelled", 0);
        progress.message = Some("Operation stopped at an async cancellation point".into());
        agent
            .db
            .set_operation_progress(operation_id, Some(&progress))?;
        for entry in agent.db.list_sync_journal_entries(operation_id, None)? {
            if entry.state == SyncJournalState::InProgress {
                agent.db.set_sync_journal_state(
                    operation_id,
                    &entry.item_key,
                    SyncJournalState::RetryScheduled,
                    Some("operation cancelled"),
                    None,
                )?;
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        tracing::error!(%operation_id, %error, "failed to persist cancelled operation");
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
        if is_terminal_state(&op.state) {
            return Ok(());
        }
        let mut metrics = operation_metrics(agent, &op)?;
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
            .insert("metrics".into(), serde_json::to_value(&metrics)?);
        agent.db.set_operation_metrics(operation_id, &metrics)?;
        let mut progress = OperationProgress::new("failed", 0);
        progress.message = op.last_error.clone();
        agent
            .db
            .set_operation_progress(operation_id, Some(&progress))?;
        agent.db.upsert_operation(&op)
    })();
    if let Err(error) = result {
        tracing::error!(%operation_id, %error, "failed to persist failed operation");
    }
}

fn reconcile_process_state_best_effort(agent: &StateAgent) {
    if let Err(error) = agent.discover() {
        tracing::warn!(%error, "application discovery failed while listing active sessions");
    }
    if let Err(error) = agent.reconcile_live_processes() {
        tracing::warn!(%error, "live process reconciliation failed while listing active sessions");
    }
    if let Err(error) = agent.process_events() {
        tracing::warn!(%error, "process event reconciliation failed while listing active sessions");
    }
}

fn active_app_sessions(db: &noland_state_db::StateDb) -> Result<GetActiveAppSessionsResult> {
    let candidates = noland_discovery::filter_backup_candidates(db.list_apps()?)
        .into_iter()
        .map(|app| (app.app_id.clone(), app))
        .collect::<HashMap<_, _>>();
    let mut sessions = Vec::new();
    for session in db.open_sessions()? {
        let Some(app) = candidates.get(&session.app_id) else {
            continue;
        };
        sessions.push(ActiveAppSession {
            session_id: session.session_id,
            app_id: session.app_id,
            display_name: app.display_name.clone(),
            pids: db.session_pids(session.session_id)?,
            started_at: session.started_at,
            identity_confidence: session.identity_confidence,
        });
    }
    Ok(GetActiveAppSessionsResult { sessions })
}

fn resolve_process_to_app(
    db: &noland_state_db::StateDb,
    pid: i32,
) -> Result<Option<ResolvedProcessApp>> {
    if let Some(session) = db.session_for_pid(pid)? {
        if let Some(app) = db
            .get_app(&session.app_id)?
            .and_then(|app| noland_discovery::filter_backup_candidates(vec![app]).pop())
        {
            return Ok(Some(ResolvedProcessApp {
                app_id: app.app_id,
                display_name: app.display_name,
                session_id: session.session_id,
                started_at: session.started_at,
                identity_confidence: session.identity_confidence,
            }));
        }
        // The database session is stale, incomplete, or filtered. Continue to
        // the executable fallback instead of treating it as authoritative.
    }

    // Prefer the indexed identity for directly launched AppImages. The kernel exe link
    // points into the transient FUSE mount, while argv[0] retains the durable AppImage
    // path that discovery indexed in shared storage.
    if let Some(indexed_executable) =
        std::fs::read(format!("/proc/{pid}/cmdline"))
            .ok()
            .and_then(|cmdline| {
                cmdline
                    .split(|byte| *byte == 0)
                    .find(|part| !part.is_empty())
                    .and_then(|part| std::str::from_utf8(part).ok())
                    .map(PathBuf::from)
            })
    {
        let indexed_apps = noland_discovery::filter_backup_candidates(db.list_apps()?);
        if let Some(app) =
            noland_discovery::resolve_identity_for_executable(&indexed_apps, &indexed_executable)
        {
            tracing::info!(
                pid,
                app_id = %app.app_id,
                executable = %indexed_executable.display(),
                "resolved process to indexed application identity"
            );
            return Ok(Some(ResolvedProcessApp {
                app_id: app.app_id,
                display_name: app.display_name,
                session_id: Uuid::new_v4(),
                started_at: Utc::now(),
                identity_confidence: app.identity_confidence,
            }));
        }
    }

    // Temporary bridge for native/AppImage processes started after discovery. The
    // attribution observer remains the source of persistent sessions; this fallback
    // only supplies the lifecycle foreground tracker with a safe executable identity.
    let executable = match std::fs::read_link(format!("/proc/{pid}/exe")) {
        Ok(executable) => PathBuf::from(executable),
        Err(error) => {
            tracing::warn!(
                pid,
                %error,
                "unable to read process executable; trying process command line"
            );
            let Some(executable) =
                std::fs::read(format!("/proc/{pid}/cmdline"))
                    .ok()
                    .and_then(|cmdline| {
                        cmdline
                            .split(|byte| *byte == 0)
                            .find(|part| !part.is_empty())
                            .and_then(|part| std::str::from_utf8(part).ok())
                            .map(PathBuf::from)
                    })
            else {
                tracing::warn!(
                    pid,
                    "unable to read process command line for lifecycle fallback"
                );
                return Ok(None);
            };
            executable
        }
    };
    let executable_name = executable
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    tracing::info!(pid, executable = %executable.display(), executable_name = %executable_name, "resolving process through executable fallback");
    if matches!(
        executable_name.as_str(),
        "systemd"
            | "systemd-journald"
            | "systemd-logind"
            | "upowerd"
            | "dbus-daemon"
            | "dbus-broker"
            | "polkitd"
            | "sshd"
            | "sunshine"
            | "pipewire"
            | "wireplumber"
            | "xorg"
    ) || executable_name.starts_with("xdg-desktop-portal")
        || executable_name.starts_with("noland-")
    {
        tracing::warn!(pid, executable_name = %executable_name, "rejecting system executable from lifecycle fallback");
        return Ok(None);
    }
    let app = noland_discovery::fallback_exe_identity(&executable);
    let now = Utc::now();
    Ok(Some(ResolvedProcessApp {
        app_id: app.app_id,
        display_name: app.display_name,
        session_id: Uuid::new_v4(),
        started_at: now,
        identity_confidence: app.identity_confidence,
    }))
}

fn verify_backup_commit(
    db: &noland_state_db::StateDb,
    app_id: AppId,
    bundle_id: uuid::Uuid,
    commit_id: uuid::Uuid,
) -> Result<VerifyBackupCommitResult> {
    let Some((latest_commit_id, latest_bundle_id, _)) = db.latest_commit(&app_id)? else {
        return Err(StateError::NotFound(format!(
            "committed backup for {app_id}"
        )));
    };
    if latest_commit_id != commit_id || latest_bundle_id != bundle_id {
        return Err(StateError::Conflict(format!(
            "latest committed backup for {app_id} is bundle {latest_bundle_id}, commit {latest_commit_id}; requested bundle {bundle_id}, commit {commit_id}"
        )));
    }
    Ok(VerifyBackupCommitResult {
        verified: true,
        app_id,
        bundle_id,
        commit_id,
    })
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

fn req_uuid(params: &serde_json::Value, key: &str) -> Result<uuid::Uuid> {
    let raw = req_str(params, key)?;
    uuid::Uuid::parse_str(&raw)
        .map_err(|error| StateError::Invalid(format!("{key} must be a UUID: {error}")))
}

fn req_pid(params: &serde_json::Value) -> Result<i32> {
    let pid = params
        .get("pid")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| StateError::Invalid("pid must be an integer".into()))?;
    i32::try_from(pid).map_err(|_| StateError::Invalid("pid must fit in a 32-bit integer".into()))
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

    fn test_session(app_id: AppId, session_id: u128, pid: i32, confidence: f32) -> AppSession {
        AppSession {
            session_id: uuid::Uuid::from_u128(session_id),
            app_id,
            root_pid: pid,
            cgroup_path: "/noland/test".into(),
            started_at: "2025-01-02T03:04:05Z".parse().unwrap(),
            ended_at: None,
            source: SessionSource::ExecutableDiscovery,
            identity_confidence: confidence,
        }
    }

    #[test]
    fn active_sessions_exclude_filtered_apps_and_include_all_session_pids() {
        let db = noland_state_db::StateDb::open_in_memory().unwrap();
        let game = AppIdentity::new(AppId::steam(480), "Spacewar");
        let filtered = AppIdentity::new(AppId::desktop("org.kde.kate"), "Kate");
        db.upsert_app(&game).unwrap();
        db.upsert_app(&filtered).unwrap();

        let game_session = test_session(game.app_id.clone(), 1, 101, 0.91);
        let filtered_session = test_session(filtered.app_id.clone(), 2, 202, 0.82);
        db.insert_session(&game_session).unwrap();
        db.attach_pid(game_session.session_id, 102, Some(101), None)
            .unwrap();
        db.insert_session(&filtered_session).unwrap();

        let result = active_app_sessions(&db).unwrap();
        assert_eq!(result.sessions.len(), 1);
        assert_eq!(result.sessions[0].session_id, game_session.session_id);
        assert_eq!(result.sessions[0].app_id, game.app_id);
        assert_eq!(result.sessions[0].display_name, "Spacewar");
        assert_eq!(result.sessions[0].pids, vec![101, 102]);
        assert_eq!(result.sessions[0].identity_confidence, 0.91);
    }

    #[test]
    fn pid_resolution_returns_only_backup_candidates() {
        let db = noland_state_db::StateDb::open_in_memory().unwrap();
        let game = AppIdentity::new(AppId::steam(480), "Spacewar");
        let filtered = AppIdentity::new(AppId::desktop("org.kde.kate"), "Kate");
        db.upsert_app(&game).unwrap();
        db.upsert_app(&filtered).unwrap();
        let game_session = test_session(game.app_id.clone(), 3, 303, 0.93);
        let filtered_session = test_session(filtered.app_id.clone(), 4, 404, 0.84);
        db.insert_session(&game_session).unwrap();
        db.insert_session(&filtered_session).unwrap();

        let resolved = resolve_process_to_app(&db, 303).unwrap().unwrap();
        assert_eq!(resolved.app_id, game.app_id);
        assert_eq!(resolved.display_name, "Spacewar");
        assert_eq!(resolved.session_id, game_session.session_id);
        assert_eq!(resolved.started_at, game_session.started_at);
        assert_eq!(resolved.identity_confidence, 0.93);
        assert!(resolve_process_to_app(&db, 404).unwrap().is_none());
        assert!(resolve_process_to_app(&db, 505).unwrap().is_none());
    }

    #[test]
    fn backup_commit_verification_requires_exact_latest_committed_ids() {
        let db = noland_state_db::StateDb::open_in_memory().unwrap();
        let app_id = AppId::steam(480);
        let first_bundle = uuid::Uuid::from_u128(10);
        let first_commit = uuid::Uuid::from_u128(11);
        let latest_bundle = uuid::Uuid::from_u128(20);
        let latest_commit = uuid::Uuid::from_u128(21);
        db.record_commit(
            first_commit,
            &app_id,
            first_bundle,
            "first",
            None,
            CommitVisibility::Committed,
        )
        .unwrap();
        db.record_commit(
            latest_commit,
            &app_id,
            latest_bundle,
            "latest",
            None,
            CommitVisibility::Committed,
        )
        .unwrap();

        let verified =
            verify_backup_commit(&db, app_id.clone(), latest_bundle, latest_commit).unwrap();
        assert!(verified.verified);
        assert_eq!(verified.app_id, app_id);
        assert_eq!(verified.bundle_id, latest_bundle);
        assert_eq!(verified.commit_id, latest_commit);
        assert!(verify_backup_commit(&db, app_id.clone(), first_bundle, first_commit).is_err());
        assert!(verify_backup_commit(
            &db,
            app_id.clone(),
            latest_bundle,
            uuid::Uuid::from_u128(22),
        )
        .is_err());

        let pending_bundle = uuid::Uuid::from_u128(30);
        let pending_commit = uuid::Uuid::from_u128(31);
        db.record_commit(
            pending_commit,
            &app_id,
            pending_bundle,
            "pending",
            None,
            CommitVisibility::Uploading,
        )
        .unwrap();
        assert!(verify_backup_commit(&db, app_id, pending_bundle, pending_commit).is_err());
    }

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
