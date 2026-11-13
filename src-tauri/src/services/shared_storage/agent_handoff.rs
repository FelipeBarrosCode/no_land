//! Push an ephemeral rclone session and invoke the state-agent.

use std::time::Duration;

use noland_rclone_adapter::EphemeralRcloneSession;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::errors::{AppError, AppResult};
use crate::models::app_state::SharedStorageObjectEntry;
use crate::models::events::{SharedStorageProgressEvent, SharedStorageRestoreCompletedEvent};
use crate::services::app_context::{ActiveAgentOperation, AppContext};
use crate::services::remote_exec::RemoteExec;
use crate::services::shared_storage::agent_runtime::{call_agent_raw, ensure_state_agent};
use crate::services::shared_storage::operation_monitor::{
    monitor_operation, select_existing_operation, ExistingOperation, MonitorFailure, OperationMatch,
};
use crate::services::shared_storage::operation_submission::{
    clear_rejected_submission, request_fingerprint, submit_with_id, SubmissionAttemptError,
    SubmissionFailure, SubmissionMemory, SubmissionReceipt,
};
use crate::services::shared_storage::provider_profiles::shared_profile_manager;
use crate::services::shared_storage::rclone_adapter::mint_ephemeral_session;
use crate::services::shared_storage::shared_storage_manager::SharedStorageManager;

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub(crate) struct AgentAppRecord {
    #[serde(alias = "appId")]
    pub app_id: String,
    #[serde(alias = "displayName")]
    pub display_name: String,
    #[serde(default, alias = "canonicalExecutable")]
    pub canonical_executable: Option<String>,
    #[serde(default, alias = "desktopEntryId")]
    pub desktop_entry_id: Option<String>,
    #[serde(default, alias = "steamAppId")]
    pub steam_app_id: Option<u32>,
    #[serde(default)]
    pub launcher: Option<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default, alias = "iconPath")]
    pub icon_path: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct AgentCatalogAppRecord {
    #[serde(alias = "appId")]
    pub app_id: String,
    #[serde(alias = "displayName")]
    pub display_name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default, alias = "canonicalExecutable")]
    pub canonical_executable: Option<String>,
    #[serde(default, alias = "desktopEntryId")]
    pub desktop_entry_id: Option<String>,
    #[serde(default, alias = "steamAppId")]
    pub steam_app_id: Option<u32>,
    #[serde(default)]
    pub launcher: Option<String>,
    #[serde(default, alias = "iconPath")]
    pub icon_path: Option<String>,
    #[serde(default, alias = "latestBundleId")]
    pub latest_bundle_id: Option<String>,
    #[serde(default, alias = "latestCompleteBundleId")]
    pub latest_complete_bundle_id: Option<String>,
    #[serde(default, alias = "latestPersonalStateBundleId")]
    pub latest_personal_state_bundle_id: Option<String>,
    #[serde(default, alias = "latestCompleteCapturedAt")]
    pub latest_complete_captured_at: Option<String>,
    #[serde(default, alias = "latestPersonalStateCapturedAt")]
    pub latest_personal_state_captured_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AgentCatalogDocument {
    #[serde(default)]
    apps: Vec<AgentCatalogAppRecord>,
}

impl SharedStorageManager {
    pub async fn prepare_agent_operation(
        context: &AppContext,
        remote: &RemoteExec,
        target_user: &str,
    ) -> AppResult<(EphemeralRcloneSession, String)> {
        Self::ensure_rclone_installed(remote).await?;
        ensure_state_agent(remote, target_user).await?;
        let session_op = Uuid::new_v4().to_string();
        // Resolve once: a profile switch during these awaits must not pair one
        // provider's session with another provider's repository key.
        let active = Self::resolve_active_profile(context).await?;
        let session = mint_ephemeral_session(
            &active.profile.provider,
            &active.credentials,
            &active.provider_fields,
            active.profile.bucket.as_deref(),
            active.profile.prefix.as_deref(),
            &active.remote_name,
            &session_op,
        )?;
        let key_hex = hex::encode(
            shared_profile_manager()
                .retrieve_repository_key(context, &active.profile)
                .await?,
        );
        Ok((session, key_hex))
    }

    pub async fn start_agent_backup(
        context: &AppContext,
        remote: &RemoteExec,
        instance_id: u64,
        target_user: &str,
        app_id: &str,
        mode: &str,
        master_key_hex: Option<&str>,
    ) -> AppResult<serde_json::Value> {
        let (session, key) = Self::prepare_agent_operation(context, remote, target_user).await?;
        let hex = master_key_hex.unwrap_or(key.as_str());
        let submission = queue_or_reattach(
            context,
            remote,
            instance_id,
            "backup",
            app_id,
            None,
            mode,
            json!({
                "app_id": app_id,
                "mode": mode,
                "client_storage_identity": operation_storage_identity(&session),
                "session": session,
                "master_key_hex": hex,
            }),
        )
        .await?;
        wait_for_agent_operation(
            context,
            remote,
            instance_id,
            &submission.operation_id,
            "backup",
            None,
            None,
            Duration::from_secs(2 * 60 * 60),
            submission.status,
        )
        .await
    }

    pub async fn start_agent_seal(
        context: &AppContext,
        remote: &RemoteExec,
        instance_id: u64,
        target_user: &str,
        mode: &str,
        master_key_hex: Option<&str>,
    ) -> AppResult<serde_json::Value> {
        let (session, key) = Self::prepare_agent_operation(context, remote, target_user).await?;
        let hex = master_key_hex.unwrap_or(key.as_str());
        let submission = queue_or_reattach(
            context,
            remote,
            instance_id,
            "seal",
            "*",
            None,
            mode,
            json!({
                "mode": mode,
                "client_storage_identity": operation_storage_identity(&session),
                "session": session,
                "master_key_hex": hex,
            }),
        )
        .await?;
        let operation_id = &submission.operation_id;
        let completed = wait_for_agent_operation(
            context,
            remote,
            instance_id,
            operation_id,
            "seal",
            None,
            None,
            Duration::from_secs(2 * 60 * 60),
            submission.status,
        )
        .await;
        let cancelled = seal_cancellation_requested(
            instance_id,
            operation_id,
            !matches!(&completed, Err(AppError::Timeout(_))),
        );
        if cancelled {
            return Err(AppError::Cancelled);
        }
        let completed = completed?;
        require_successful_seal(&completed)?;
        Ok(completed)
    }

    pub async fn seal_before_stop(
        context: &AppContext,
        remote: &RemoteExec,
        instance_id: u64,
        target_user: &str,
    ) -> AppResult<serde_json::Value> {
        Self::start_agent_seal(
            context,
            remote,
            instance_id,
            target_user,
            "personal_state",
            None,
        )
        .await
    }

    pub(crate) fn request_seal_cancellation(instance_id: u64, operation_id: &str) {
        CANCELLED_SEALS
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert((instance_id, operation_id.to_owned()));
    }

    pub async fn list_agent_catalog(
        context: &AppContext,
        remote: &RemoteExec,
        target_user: &str,
        master_key_hex: Option<&str>,
    ) -> AppResult<serde_json::Value> {
        let (session, key) = Self::prepare_agent_operation(context, remote, target_user).await?;
        let hex = master_key_hex.unwrap_or(key.as_str());
        call_agent_raw(
            remote,
            "ListCloudCatalog",
            json!({
                "session": session,
                "master_key_hex": hex,
            }),
        )
        .await
    }

    pub async fn start_agent_restore(
        context: &AppContext,
        remote: &RemoteExec,
        instance_id: u64,
        target_user: &str,
        app_id: &str,
        bundle_id: &str,
        mode: &str,
    ) -> AppResult<serde_json::Value> {
        let (session, hex) = Self::prepare_agent_operation(context, remote, target_user).await?;
        let submission = queue_or_reattach(
            context,
            remote,
            instance_id,
            "restore",
            app_id,
            Some(bundle_id),
            mode,
            json!({
                "app_id": app_id,
                "bundle_id": bundle_id,
                "mode": mode,
                "client_storage_identity": operation_storage_identity(&session),
                "session": session,
                "master_key_hex": hex,
            }),
        )
        .await?;
        wait_for_agent_operation(
            context,
            remote,
            instance_id,
            &submission.operation_id,
            "restore",
            Some(app_id),
            Some(bundle_id),
            Duration::from_secs(2 * 60 * 60),
            submission.status,
        )
        .await
    }

    pub async fn list_agent_apps(
        _context: &AppContext,
        remote: &RemoteExec,
        target_user: &str,
    ) -> AppResult<Vec<SharedStorageObjectEntry>> {
        ensure_state_agent(remote, target_user).await?;
        let apps = call_agent_raw(remote, "ListApps", json!({})).await?;
        Ok(apps_to_entries(&apps))
    }

    pub async fn catalog_to_entries(
        context: &AppContext,
        remote: &RemoteExec,
        target_user: &str,
    ) -> AppResult<Vec<SharedStorageObjectEntry>> {
        let catalog = Self::list_agent_catalog(context, remote, target_user, None).await?;
        Ok(catalog_to_entries(&catalog))
    }

    pub(crate) async fn list_agent_app_records(
        remote: &RemoteExec,
        target_user: &str,
    ) -> AppResult<Vec<AgentAppRecord>> {
        ensure_state_agent(remote, target_user).await?;
        let apps = call_agent_raw(remote, "ListApps", json!({})).await?;
        parse_agent_apps(&apps)
    }

    pub(crate) async fn list_agent_catalog_records(
        context: &AppContext,
        remote: &RemoteExec,
        target_user: &str,
    ) -> AppResult<Vec<AgentCatalogAppRecord>> {
        let catalog = Self::list_agent_catalog(context, remote, target_user, None).await?;
        parse_agent_catalog(&catalog)
    }
}

fn operation_storage_identity(session: &EphemeralRcloneSession) -> serde_json::Value {
    // Preserve the exact root: e.g. SFTP /repo and repo are different locations.
    // Credentials and ephemeral session IDs must never enter persisted matching data.
    json!({
        "provider": session.provider,
        "backend_type": session.backend_type,
        "remote_name": session.remote_name,
        "root": session.root,
    })
}

// Serialize the lookup/submission boundary, not the long-running monitor. Two
// UI requests must not both observe an empty operation list and submit work.
static SUBMISSION: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static SUBMISSION_MEMORY: SubmissionMemory = SubmissionMemory::new();
static CANCELLED_SEALS: std::sync::Mutex<std::collections::BTreeSet<(u64, String)>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

fn seal_cancellation_requested(instance_id: u64, operation_id: &str, terminal: bool) -> bool {
    let mut cancelled = CANCELLED_SEALS
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let key = (instance_id, operation_id.to_owned());
    if terminal {
        cancelled.remove(&key)
    } else {
        cancelled.contains(&key)
    }
}

async fn queue_or_reattach(
    context: &AppContext,
    remote: &RemoteExec,
    instance_id: u64,
    kind: &str,
    app_id: &str,
    bundle_id: Option<&str>,
    mode: &str,
    mut params: serde_json::Value,
) -> AppResult<SubmissionReceipt> {
    let _submission = SUBMISSION.lock().await;
    let fingerprint = request_fingerprint(kind, &params);
    // A newer request of the same kind must not mask an earlier paused ID.
    // Only this exact immutable fingerprint authorizes an unobserved replay.
    let local_resume_id = SUBMISSION_MEMORY.lookup(instance_id, &fingerprint);
    let preferred_id = match &local_resume_id {
        Some(id) => Some(id.clone()),
        None => context
            .active_agent_operation(instance_id, Some(kind))
            .await
            .map(|operation| operation.operation_id),
    };
    let request = OperationMatch {
        kind,
        app_id,
        bundle_id,
        mode,
        storage_identity: &params["client_storage_identity"],
        regenerable_caches: params
            .get("regenerable_caches")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("include"),
    };
    let existing = match find_existing_operation(
        remote,
        &request,
        preferred_id.as_deref(),
        local_resume_id.is_some(),
    )
    .await
    {
        Ok(existing) => existing,
        // A missing/unreachable remote record is ambiguous. Only our retained
        // immutable fingerprint authorizes replaying the original ID here.
        Err(OperationLookupFailure::Unavailable(_)) if local_resume_id.is_some() => None,
        Err(
            OperationLookupFailure::Unavailable(error) | OperationLookupFailure::Mismatch(error),
        ) => return Err(error),
    };
    if let Some(existing) = &existing {
        let (ExistingOperation::Running(id)
        | ExistingOperation::Tracked(id)
        | ExistingOperation::Interrupted(id)
        | ExistingOperation::UnboundRunning(id)) = existing;
        if SUBMISSION_MEMORY.has_conflicting_binding(instance_id, id, &fingerprint) {
            return Err(AppError::State(format!("Operation {id} is retained for different selection, provider or vault inputs; submission deferred")));
        }
    }
    let matched_seal_id = match existing {
        Some(ExistingOperation::Running(id) | ExistingOperation::Tracked(id)) if kind == "seal" => Some(id),
        Some(ExistingOperation::Running(id) | ExistingOperation::Tracked(id)) => return Ok(SubmissionReceipt::attached(id)),
        Some(ExistingOperation::UnboundRunning(id)) => return Err(AppError::State(format!(
            "Operation {id} is still running but its provider cannot be verified. Resume or cancel that operation before submitting another {kind}."
        ))),
        Some(ExistingOperation::Interrupted(id)) => {
            if kind == "seal" {
                // Resuming a pre-stop monitor cannot restart a failed/interrupted
                // lifecycle prerequisite and subsequently authorize destruction.
                return Ok(SubmissionReceipt::attached(id));
            }
            // A retry is allowed only after an authoritative INTERRUPTED result
            // with matching app/bundle/mode/provider. A lost retry response is
            // resolved by monitoring this ID, never by another submission.
            let queued = call_agent_raw(remote, "RetryOperation", json!({
                "operation_id": id,
                "session": params["session"],
                "master_key_hex": params["master_key_hex"],
            })).await;
            let status = if let Ok(queued) = queued {
                if queued.get("operation_id").and_then(serde_json::Value::as_str).is_some_and(|returned| returned != id) {
                    return Err(AppError::State(format!("Retry returned a different operation ID; original resumable operation is {id}")));
                }
                Some(queued)
            } else { None };
            return Ok(SubmissionReceipt { operation_id: id, status });
        }
        None => None,
    };
    let method = match kind {
        "backup" => "StartBackup",
        "restore" => "StartRestore",
        "seal" => "StartSeal",
        _ => {
            return Err(AppError::InvalidInput(
                "Unsupported state-agent operation kind".into(),
            ))
        }
    };
    let operation_id = matched_seal_id
        .or(local_resume_id)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    params["client_operation_id"] = operation_id.clone().into();
    if !SUBMISSION_MEMORY.remember(instance_id, &operation_id, fingerprint) {
        return Err(AppError::State(format!("Operation {operation_id} is retained for different selection, provider or vault inputs; submission deferred")));
    }
    let result = submit_with_id(
        operation_id.clone(),
        |id| async move {
            context
                .retain_agent_operation(ActiveAgentOperation {
                    instance_id,
                    operation_id: id,
                    kind: kind.to_owned(),
                })
                .await;
        },
        |id| {
            let params = params.clone();
            async move {
                // Once cancellation is requested, reconcile only. An admission
                // retry must not start an as-yet-unaccepted seal after cancellation.
                if kind == "seal" && seal_cancellation_requested(instance_id, &id, false) {
                    return call_agent_raw(
                        remote,
                        "GetOperationStatus",
                        json!({"operation_id": id, "projection": "progress"}),
                    )
                    .await
                    .map_err(|_| SubmissionAttemptError::Unavailable);
                }
                call_agent_raw(remote, method, params)
                    .await
                    .map_err(|error| {
                        // call_agent_raw distinguishes an authoritative RPC error
                        // using this prefix; SSH/transfer/decoding errors do not use it.
                        match error {
                            AppError::Provisioning(message)
                                if message.starts_with(&format!("state-agent {method}:")) =>
                            {
                                SubmissionAttemptError::Rejected(message)
                            }
                            _ => SubmissionAttemptError::Unavailable,
                        }
                    })
            }
        },
        |status| emit_operation_progress(context, instance_id, kind, &operation_id, status),
    )
    .await;
    if clear_rejected_submission(
        &result,
        &SUBMISSION_MEMORY,
        instance_id,
        &operation_id,
        context.clear_agent_operation(instance_id, &operation_id),
    )
    .await
    {
        seal_cancellation_requested(instance_id, &operation_id, true);
        emit_operation_progress(
            context,
            instance_id,
            kind,
            &operation_id,
            &json!({
                "state": "FAILED", "running": false,
                "progress": {"phase": "rejected", "message": "Operation submission was rejected"},
            }),
        );
    }
    result.map_err(|error| match error {
        SubmissionFailure::Unconfirmed(message) | SubmissionFailure::Incompatible(message) => {
            AppError::Timeout(message)
        }
        SubmissionFailure::Rejected(message) => AppError::State(message),
    })
}

enum OperationLookupFailure {
    Unavailable(AppError),
    Mismatch(AppError),
}

async fn find_existing_operation(
    remote: &RemoteExec,
    request: &OperationMatch<'_>,
    preferred_id: Option<&str>,
    require_preferred_match: bool,
) -> Result<Option<ExistingOperation>, OperationLookupFailure> {
    let recent = call_agent_raw(remote, "ListRecentOperations", json!({
        "limit": 100, "projection": "matching", "kind": request.kind,
        "app_id": if matches!(request.app_id, "*" | "_all") { None } else { Some(request.app_id) },
        "mode": request.mode, "bundle_id": request.bundle_id,
        "client_storage_identity": request.storage_identity,
        "regenerable_caches": request.regenerable_caches, "preferred_id": preferred_id,
    })).await.map_err(OperationLookupFailure::Unavailable)?;
    let mut operations = recent
        .get("operations")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .ok_or_else(|| {
            AppError::State(
                "state-agent returned an invalid operation list; submission deferred".into(),
            )
        })
        .map_err(OperationLookupFailure::Unavailable)?;
    if let Some(id) = preferred_id {
        if !operations.iter().any(|operation| {
            operation
                .get("operation_id")
                .and_then(serde_json::Value::as_str)
                == Some(id)
        }) {
            let status = call_agent_raw(
                remote,
                "GetOperationStatus",
                json!({"operation_id": id, "projection": "matching"}),
            )
            .await
            .map_err(OperationLookupFailure::Unavailable)?;
            if status
                .get("operation_id")
                .and_then(serde_json::Value::as_str)
                != Some(id)
            {
                return Err(OperationLookupFailure::Mismatch(AppError::State(format!(
                    "Could not verify active operation {id}; submission deferred"
                ))));
            }
            operations.insert(0, status);
        }
        if require_preferred_match {
            let preferred = operations
                .iter()
                .find(|operation| operation["operation_id"].as_str() == Some(id))
                .expect("preferred status was found or fetched");
            match select_existing_operation(std::slice::from_ref(preferred), request, Some(id)) {
                Some(unbound @ ExistingOperation::UnboundRunning(_)) => return Ok(Some(unbound)),
                Some(_) => {},
                None => return Err(OperationLookupFailure::Mismatch(AppError::State(format!(
                    "Retained operation {id} has mismatched or unverifiable status; submission deferred"
                )))),
            }
        }
    }
    Ok(select_existing_operation(
        &operations,
        request,
        preferred_id,
    ))
}

fn require_successful_seal(status: &serde_json::Value) -> AppResult<()> {
    if status
        .get("cancel_requested")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        return Err(AppError::Cancelled);
    }
    match status.get("state").and_then(serde_json::Value::as_str) {
        Some("SEALED" | "COMPLETED") => Ok(()),
        _ => Err(AppError::Provisioning(
            "Pre-stop backup has not completed successfully; refusing to stop the instance.".into(),
        )),
    }
}

pub(super) async fn wait_for_agent_operation(
    context: &AppContext,
    remote: &RemoteExec,
    instance_id: u64,
    operation_id: &str,
    kind: &str,
    app_id: Option<&str>,
    bundle_id: Option<&str>,
    timeout: Duration,
    initial_status: Option<serde_json::Value>,
) -> AppResult<serde_json::Value> {
    context
        .retain_agent_operation(ActiveAgentOperation {
            instance_id,
            operation_id: operation_id.to_string(),
            kind: kind.to_string(),
        })
        .await;
    let result = monitor_operation(
        operation_id,
        timeout,
        initial_status,
        |id| async move {
            call_agent_raw(
                remote,
                "GetOperationStatus",
                json!({"operation_id": id, "projection": "progress"}),
            )
            .await
        },
        |status| emit_operation_progress(context, instance_id, kind, operation_id, status),
    )
    .await;
    // A monitoring timeout is not a terminal execution result. Retain the
    // cancellation/resumption handle until authoritative completion is observed.
    if !result
        .as_ref()
        .err()
        .is_some_and(MonitorFailure::keep_active_handle)
    {
        context
            .clear_agent_operation(instance_id, operation_id)
            .await;
        SUBMISSION_MEMORY.forget(instance_id, operation_id);
    }
    let status = result.map_err(|error| match error {
        MonitorFailure::TimedOut { message } => AppError::Timeout(message),
        MonitorFailure::Terminal { state, message } if state == "FAILED" => {
            AppError::Provisioning(message)
        }
        MonitorFailure::Terminal { message, .. } => AppError::State(message),
    })?;
    if kind == "restore"
        && status.get("state").and_then(serde_json::Value::as_str) == Some("COMPLETED")
    {
        if let (Some(app_id), Some(bundle_id)) = (app_id, bundle_id) {
            context.emit_shared_storage_restore_completed(SharedStorageRestoreCompletedEvent {
                operation_id: operation_id.to_string(),
                instance_id,
                kind: kind.to_string(),
                app_id: app_id.to_string(),
                display_name: app_id.to_string(),
                bundle_id: bundle_id.to_string(),
            });
        }
    }
    Ok(status)
}

fn emit_operation_progress(
    context: &AppContext,
    instance_id: u64,
    kind: &str,
    operation_id: &str,
    status: &serde_json::Value,
) {
    context.emit_shared_storage_progress(operation_progress_event(
        instance_id,
        kind,
        operation_id,
        status,
    ));
}

fn operation_progress_event(
    instance_id: u64,
    kind: &str,
    operation_id: &str,
    status: &serde_json::Value,
) -> SharedStorageProgressEvent {
    let progress = status.get("progress");
    let backup_pipeline = progress
        .and_then(|p| p.pointer("/detail_json/streaming_pipeline"))
        .cloned();
    let deletion = (kind == "delete")
        .then(|| {
            progress
                .and_then(|p| p.pointer("/detail_json/deletion"))
                .cloned()
        })
        .flatten();
    let phase = progress
        .and_then(|value| value.get("phase"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let message = progress
        .and_then(|value| value.get("message"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            status
                .get("state")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        });
    let completed_units = progress
        .and_then(|value| value.get("completed_units"))
        .and_then(serde_json::Value::as_u64);
    let total_units = progress
        .and_then(|value| value.get("total_units"))
        .and_then(serde_json::Value::as_u64);
    let unit = progress
        .and_then(|value| value.get("unit"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let fraction = if backup_pipeline.is_some() {
        None
    } else {
        match (completed_units, total_units) {
            (Some(completed), Some(total)) if total > 0 => {
                Some((completed as f64 / total as f64).clamp(0.0, 1.0))
            }
            _ => None,
        }
    };
    let pack_transfer = status.get("pack_transfer");
    let completed_objects = deletion
        .as_ref()
        .and_then(|value| value.get("completed_packs"))
        .and_then(serde_json::Value::as_u64)
        .or_else(|| {
            pack_transfer
                .and_then(|value| value.get("completed_items"))
                .and_then(serde_json::Value::as_u64)
        });
    let total_objects = deletion
        .as_ref()
        .and_then(|value| value.get("total_packs"))
        .and_then(serde_json::Value::as_u64)
        .or_else(|| {
            progress
                .and_then(|value| value.get("detail_json"))
                .and_then(|value| value.get("total_packs"))
                .and_then(serde_json::Value::as_u64)
                .or_else(|| {
                    pack_transfer
                        .and_then(|value| value.get("total_items"))
                        .and_then(serde_json::Value::as_u64)
                })
        });
    let pipeline = progress
        .and_then(|value| value.get("detail_json"))
        .and_then(|value| value.get("pipeline"));
    let completed_bytes = deletion
        .as_ref()
        .and_then(|value| value.get("verified_absent_bytes"))
        .and_then(serde_json::Value::as_u64)
        .or_else(|| {
            pipeline
                .and_then(|value| value.get("published_bytes"))
                .and_then(serde_json::Value::as_u64)
                .or_else(|| {
                    progress
                        .and_then(|value| value.get("detail_json"))
                        .and_then(|value| value.get("completed_pack_bytes"))
                        .and_then(serde_json::Value::as_u64)
                        .max(
                            pack_transfer
                                .and_then(|value| value.get("completed_bytes"))
                                .and_then(serde_json::Value::as_u64),
                        )
                })
        });
    let transferred_bytes = progress
        .and_then(|value| value.get("detail_json"))
        .and_then(|value| value.get("bytes_transferred"))
        .and_then(serde_json::Value::as_u64)
        .max(
            pack_transfer
                .and_then(|value| value.get("bytes_transferred"))
                .and_then(serde_json::Value::as_u64),
        );
    let total_bytes = deletion
        .as_ref()
        .and_then(|value| value.get("total_bytes"))
        .and_then(serde_json::Value::as_u64)
        .or_else(|| {
            pipeline
                .and_then(|value| value.get("total_bytes"))
                .and_then(serde_json::Value::as_u64)
                .or_else(|| {
                    progress
                        .and_then(|value| value.get("detail_json"))
                        .and_then(|value| value.get("total_transfer_bytes"))
                        .and_then(serde_json::Value::as_u64)
                        .max(
                            pack_transfer
                                .and_then(|value| value.get("total_bytes"))
                                .and_then(serde_json::Value::as_u64),
                        )
                })
        });
    let object_unit = total_objects
        .filter(|total| *total > 0)
        .map(|_| "packs".to_string());
    let ready_to_launch = phase.as_deref() == Some("READY_TO_LAUNCH")
        || progress
            .and_then(|value| value.get("detail_json"))
            .and_then(|value| value.get("ready_to_launch_reached"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        || progress
            .and_then(|value| value.get("detail_json"))
            .and_then(|value| value.get("milestones"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|milestones| {
                milestones
                    .iter()
                    .any(|value| value.as_str() == Some("READY_TO_LAUNCH"))
            });
    let state = status
        .get("state")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("UNKNOWN")
        .to_string();
    let cancel_requested = status
        .get("cancel_requested")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let running = status
        .get("running")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    SharedStorageProgressEvent {
        operation_id: operation_id.to_string(),
        instance_id,
        kind: kind.to_string(),
        state,
        phase,
        message,
        completed_units,
        total_units,
        unit,
        fraction,
        completed_objects,
        total_objects,
        object_unit,
        completed_bytes,
        total_bytes,
        transferred_bytes,
        backup_pipeline,
        deletion,
        ready_to_launch,
        cancel_requested,
        cancellable: running && !cancel_requested,
    }
}

fn parse_agent_apps(value: &serde_json::Value) -> AppResult<Vec<AgentAppRecord>> {
    serde_json::from_value::<Vec<AgentAppRecord>>(value.clone())
        .map(|apps| apps.into_iter().map(AgentAppRecord::normalize).collect())
        .map_err(|error| {
            AppError::State(format!("state-agent ListApps payload is invalid: {error}"))
        })
}

fn parse_agent_catalog(value: &serde_json::Value) -> AppResult<Vec<AgentCatalogAppRecord>> {
    serde_json::from_value::<AgentCatalogDocument>(value.clone())
        .map(|catalog| {
            catalog
                .apps
                .into_iter()
                .map(AgentCatalogAppRecord::normalize)
                .collect()
        })
        .map_err(|error| {
            AppError::State(format!(
                "state-agent ListCloudCatalog payload is invalid: {error}"
            ))
        })
}

impl AgentAppRecord {
    fn normalize(mut self) -> Self {
        self.display_name = self.display_name.trim().to_string();
        self.canonical_executable = normalized_optional_string(self.canonical_executable.take());
        self.desktop_entry_id = normalized_optional_string(self.desktop_entry_id.take());
        self.launcher = normalized_launcher(self.launcher.take());
        self.steam_app_id = self
            .steam_app_id
            .or_else(|| infer_steam_app_id(&self.app_id, self.launcher.as_deref()));
        if self.launcher.is_none() && steam_like_app_id(&self.app_id, self.steam_app_id) {
            self.launcher = Some("steam".to_string());
        }
        self.aliases = normalized_aliases(self.aliases, &self.display_name);
        self.icon_path = normalized_optional_string(self.icon_path.take());
        self
    }
}

impl AgentCatalogAppRecord {
    pub(crate) fn personal_state_overlay_bundle_id(&self) -> Option<&str> {
        let complete_at = self
            .latest_complete_captured_at
            .as_deref()?
            .parse::<chrono::DateTime<chrono::Utc>>()
            .ok()?;
        let personal_at = self
            .latest_personal_state_captured_at
            .as_deref()?
            .parse::<chrono::DateTime<chrono::Utc>>()
            .ok()?;
        (personal_at > complete_at)
            .then(|| self.latest_personal_state_bundle_id.as_deref())
            .flatten()
    }

    fn normalize(mut self) -> Self {
        self.display_name = self.display_name.trim().to_string();
        self.canonical_executable = normalized_optional_string(self.canonical_executable.take());
        self.desktop_entry_id = normalized_optional_string(self.desktop_entry_id.take());
        self.launcher = normalized_launcher(self.launcher.take());
        self.steam_app_id = self
            .steam_app_id
            .or_else(|| infer_steam_app_id(&self.app_id, self.launcher.as_deref()));
        if self.launcher.is_none() && steam_like_app_id(&self.app_id, self.steam_app_id) {
            self.launcher = Some("steam".to_string());
        }
        self.aliases = normalized_aliases(self.aliases, &self.display_name);
        self.icon_path = normalized_optional_string(self.icon_path.take());
        self.latest_bundle_id = normalized_optional_string(self.latest_bundle_id.take());
        self.latest_complete_bundle_id =
            normalized_optional_string(self.latest_complete_bundle_id.take());
        self.latest_personal_state_bundle_id =
            normalized_optional_string(self.latest_personal_state_bundle_id.take());
        self.latest_complete_captured_at =
            normalized_optional_string(self.latest_complete_captured_at.take());
        self.latest_personal_state_captured_at =
            normalized_optional_string(self.latest_personal_state_captured_at.take());
        self
    }
}

fn normalized_optional_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

fn normalized_launcher(value: Option<String>) -> Option<String> {
    normalized_optional_string(value).map(|value| value.to_ascii_lowercase())
}

fn normalized_aliases(aliases: Vec<String>, display_name: &str) -> Vec<String> {
    let mut normalized = Vec::new();
    for alias in aliases {
        let trimmed = alias.trim();
        if trimmed.is_empty()
            || trimmed.eq_ignore_ascii_case(display_name)
            || normalized
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(trimmed))
        {
            continue;
        }
        normalized.push(trimmed.to_string());
    }
    normalized
}

fn infer_steam_app_id(app_id: &str, launcher: Option<&str>) -> Option<u32> {
    let app_id = app_id.trim();
    if let Some(value) = app_id.strip_prefix("steam:") {
        return value.parse().ok();
    }
    launcher
        .filter(|value| value.trim().eq_ignore_ascii_case("steam"))
        .and_then(|_| app_id.parse().ok())
}

fn steam_like_app_id(app_id: &str, steam_app_id: Option<u32>) -> bool {
    steam_app_id.is_some() || app_id.trim().starts_with("steam:")
}

fn looks_like_image_utility(id: &str, name: &str) -> bool {
    let hay = format!("{} {}", id, name).to_ascii_lowercase();
    const KEEP: &[&str] = &[
        "steam",
        "lutris",
        "heroic",
        "bottles",
        "playonlinux",
        "q4wine",
        "winetricks",
        "wine",
        "proton",
        "minecraft",
        "epic",
        "gog",
        "itch",
    ];
    if KEEP.iter().any(|m| hay.contains(m)) {
        return false;
    }
    const DROP: &[&str] = &[
        "kate",
        "dolphin",
        "konsole",
        "kwrite",
        "spectacle",
        "okular",
        "gwenview",
        "kmail",
        "korganizer",
        "kaddressbook",
        "knotes",
        "kcalc",
        "kfind",
        "klipper",
        "konqueror",
        "sweeper",
        "akregator",
        "ark",
        "juk",
        "dragon",
        "bluetooth",
        "muon",
        "discover",
        "systemsettings",
        "system settings",
        "htop",
        "vim",
        "systemd",
        "sunshine",
        "nvidia",
        "byobu",
        "texinfo",
        "idle",
        "imagemagick",
        "kde connect",
        "kwallet",
        "partition",
        "info center",
        "help",
        "emoji",
        "qsynth",
        "sieve",
        "ktnef",
        "contact print",
        "contact theme",
        "software sources",
        "additional drivers",
        "terminal",
        "add bluetooth",
    ];
    DROP.iter().any(|m| hay.contains(m))
}

fn apps_to_entries(apps: &serde_json::Value) -> Vec<SharedStorageObjectEntry> {
    let mut out = vec![SharedStorageObjectEntry {
        path: "/apps".into(),
        name: "Applications".into(),
        parent_path: "/".into(),
        is_dir: true,
    }];
    let list = apps.as_array().cloned().unwrap_or_default();
    for app in list {
        let id = app
            .get("app_id")
            .and_then(|v| v.as_str())
            .or_else(|| app.get("appId").and_then(|v| v.as_str()))
            .unwrap_or("unknown");
        let name = app
            .get("display_name")
            .and_then(|v| v.as_str())
            .or_else(|| app.get("displayName").and_then(|v| v.as_str()))
            .unwrap_or(id);
        if looks_like_image_utility(id, name) {
            continue;
        }
        out.push(SharedStorageObjectEntry {
            path: format!("/apps/{id}"),
            name: name.into(),
            parent_path: "/apps".into(),
            is_dir: false,
        });
    }
    out.insert(
        1,
        SharedStorageObjectEntry {
            path: "/apps/*".into(),
            name: "All discovered applications".into(),
            parent_path: "/apps".into(),
            is_dir: false,
        },
    );
    out
}

fn catalog_to_entries(catalog: &serde_json::Value) -> Vec<SharedStorageObjectEntry> {
    let mut retained = Vec::new();
    let mut out = vec![SharedStorageObjectEntry {
        path: "/catalog".into(),
        name: "Shared Storage apps".into(),
        parent_path: "/".into(),
        is_dir: true,
    }];
    let apps = catalog
        .get("apps")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    for app in apps {
        let id = app
            .get("app_id")
            .and_then(|v| v.as_str())
            .or_else(|| app.get("appId").and_then(|v| v.as_str()))
            .unwrap_or("unknown");
        let name = app
            .get("display_name")
            .and_then(|v| v.as_str())
            .or_else(|| app.get("displayName").and_then(|v| v.as_str()))
            .unwrap_or(id);
        let app_path = format!("/catalog/{id}");
        out.push(SharedStorageObjectEntry {
            path: app_path.clone(),
            name: name.into(),
            parent_path: "/catalog".into(),
            is_dir: true,
        });
        if let Some(bundles) = app.get("bundles").and_then(|v| v.as_array()) {
            let current = app
                .get("latest_complete_bundle_id")
                .and_then(|v| v.as_str())
                .or_else(|| app.get("latest_bundle_id").and_then(|v| v.as_str()));
            let mut has_retained = false;
            for bundle in bundles {
                let bundle_id = bundle
                    .get("bundle_id")
                    .and_then(|v| v.as_str())
                    .or_else(|| bundle.get("bundleId").and_then(|v| v.as_str()))
                    .unwrap_or("latest");
                if current.is_some_and(|id| id != bundle_id) {
                    if !has_retained {
                        retained.push(SharedStorageObjectEntry {
                            path: format!("/retained-backups/{id}"),
                            name: name.into(),
                            parent_path: "/retained-backups".into(),
                            is_dir: true,
                        });
                        has_retained = true;
                    }
                    retained.push(SharedStorageObjectEntry {
                        path: format!("/retained-backups/{id}/{bundle_id}"),
                        name: bundle_id.into(),
                        parent_path: format!("/retained-backups/{id}"),
                        is_dir: false,
                    });
                    continue;
                }
                out.push(SharedStorageObjectEntry {
                    path: format!("{app_path}/{bundle_id}"),
                    name: format!("bundle {bundle_id}"),
                    parent_path: app_path.clone(),
                    is_dir: false,
                });
            }
        } else if let Some(latest) = app
            .get("latest_bundle_id")
            .and_then(|v| v.as_str())
            .or_else(|| app.get("latestBundleId").and_then(|v| v.as_str()))
        {
            out.push(SharedStorageObjectEntry {
                path: format!("{app_path}/{latest}"),
                name: "latest bundle".into(),
                parent_path: app_path,
                is_dir: false,
            });
        }
    }
    if !retained.is_empty() {
        out.push(SharedStorageObjectEntry {
            path: "/retained-backups".into(),
            name: "Retained backups".into(),
            parent_path: "/".into(),
            is_dir: true,
        });
        out.extend(retained);
    }
    if let Some(pending) = catalog.get("pending_deletions").and_then(|v| v.as_array()) {
        if !pending.is_empty() {
            out.push(SharedStorageObjectEntry {
                path: "/pending-deletions".into(),
                name: "Pending cloud deletions".into(),
                parent_path: "/".into(),
                is_dir: true,
            });
        }
        for plan in pending {
            if let Some(id) = plan.get("plan_id").and_then(|v| v.as_str()) {
                out.push(SharedStorageObjectEntry {
                    path: format!("/pending-deletions/{id}"),
                    name: format!("Resume deletion {id}"),
                    parent_path: "/pending-deletions".into(),
                    is_dir: false,
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{parse_agent_apps, parse_agent_catalog, require_successful_seal};
    use serde_json::json;

    #[test]
    fn catalog_selector_has_one_current_head_and_delete_only_recovery_entries() {
        let entries =
            super::catalog_to_entries(&json!({"apps":[{"app_id":"game","display_name":"Game",
            "latest_bundle_id":"current","latest_complete_bundle_id":"current",
            "bundles":[{"bundle_id":"old"},{"bundle_id":"current"}]}],
            "pending_deletions":[{"plan_id":"pending"}]}));
        let restorable = entries
            .iter()
            .filter(|entry| !entry.is_dir && entry.path.starts_with("/catalog/"))
            .collect::<Vec<_>>();
        assert_eq!(restorable.len(), 1);
        assert_eq!(restorable[0].path, "/catalog/game/current");
        assert!(entries
            .iter()
            .any(|entry| entry.path == "/retained-backups/game/old"));
        assert!(entries
            .iter()
            .any(|entry| entry.path == "/pending-deletions/pending"));
    }

    #[test]
    fn progress_keeps_logical_reuse_separate_from_network_transfer() {
        let reused = json!({"state":"COMMITTING", "progress":{"detail_json":{
            "completed_pack_bytes":100,"bytes_transferred":0}},
            "pack_transfer":{"completed_bytes":900,"total_bytes":900,"bytes_transferred":0}});
        let event = super::operation_progress_event(42, "backup", "op", &reused);
        assert_eq!(event.completed_bytes, Some(900));
        assert_eq!(event.total_bytes, Some(900));
        assert_eq!(event.transferred_bytes, Some(0));
        let mut restored = reused;
        restored["progress"]["detail_json"]["pipeline"] =
            json!({"published_bytes":2000,"total_bytes":3000});
        restored["progress"]["detail_json"]["bytes_transferred"] = json!(40);
        restored["pack_transfer"]["bytes_transferred"] = json!(60);
        let event = super::operation_progress_event(42, "restore", "op", &restored);
        assert_eq!(event.completed_bytes, Some(2000));
        assert_eq!(event.total_bytes, Some(3000));
        assert_eq!(event.transferred_bytes, Some(60));
    }

    #[test]
    fn deletion_event_preserves_verified_absence_without_calling_it_network_transfer() {
        let status = json!({"state":"RUNNING", "running":true, "progress": {
        "phase":"deleting_packs", "completed_units":41, "total_units":119,
        "unit":"objects", "detail_json":{"deletion":{
            "completed_packs":37,"total_packs":115,"verified_absent_bytes":9000,"total_bytes":26000
        }}}});
        let event = super::operation_progress_event(42, "delete", "plan", &status);
        assert_eq!(event.completed_objects, Some(37));
        assert_eq!(event.total_objects, Some(115));
        assert_eq!(event.completed_bytes, Some(9000));
        assert_eq!(event.transferred_bytes, None);
        assert_eq!(event.deletion.unwrap()["completed_packs"], 37);
    }

    #[test]
    fn streaming_progress_keeps_selected_scope_separate_from_partial_pack_totals() {
        let status = json!({"state":"CAPTURING_AND_UPLOADING", "running":true,
            "progress":{"phase":"capture_and_upload", "completed_units":178, "total_units":178,"unit":"files",
                "detail_json":{"streaming_pipeline":{"selected_files":178,"source_total_bytes":24_128_523_743u64,
                    "prepared_bytes":151_000_000,"durable_bytes":900_000_000}}},
            "pack_transfer":{"total_items":2,"completed_items":1,"bytes_transferred":20_000_000}});
        let event = super::operation_progress_event(42, "backup", "op", &status);
        assert_eq!(event.fraction, None, "capture EOF is not backup completion");
        assert_eq!(event.total_objects, Some(2));
        assert_eq!(
            event.backup_pipeline.as_ref().unwrap()["selected_files"],
            178
        );
        assert_eq!(
            event.backup_pipeline.as_ref().unwrap()["source_total_bytes"],
            24_128_523_743u64
        );
        assert_eq!(event.transferred_bytes, Some(20_000_000));
    }

    #[test]
    fn uncertain_or_cancel_requested_seals_never_authorize_lifecycle_actions() {
        for state in [
            "RECONNECTING",
            "MONITORING_PAUSED",
            "INTERRUPTED",
            "READY_TO_LAUNCH",
            "QUEUED",
            "CANCELLED",
            "ROLLED_BACK",
            "FAILED",
        ] {
            assert!(require_successful_seal(&json!({"state":state})).is_err());
        }
        for state in ["COMPLETED", "SEALED"] {
            assert!(
                require_successful_seal(&json!({"state":state,"cancel_requested":true})).is_err()
            );
            assert!(
                require_successful_seal(&json!({"state":state,"cancel_requested":false})).is_ok()
            );
        }
    }

    #[test]
    fn local_seal_cancellation_survives_a_timeout_and_is_scoped_to_exact_operation() {
        let id = uuid::Uuid::new_v4().to_string();
        super::SharedStorageManager::request_seal_cancellation(42, &id);
        assert!(super::seal_cancellation_requested(42, &id, false));
        assert!(!super::seal_cancellation_requested(43, &id, true));
        assert!(!super::seal_cancellation_requested(42, "other", true));
        assert!(super::seal_cancellation_requested(42, &id, true));
        assert!(!super::seal_cancellation_requested(42, &id, false));
    }

    #[test]
    fn queued_failed_or_rolled_back_seals_never_authorize_a_stop() {
        for state in [
            "QUEUED",
            "REQUESTED",
            "FAILED",
            "CANCELLED",
            "ROLLED_BACK",
            "UNKNOWN",
        ] {
            assert!(require_successful_seal(&json!({"state": state})).is_err());
        }
        assert!(require_successful_seal(&json!({})).is_err());
        for state in ["SEALED", "COMPLETED"] {
            assert!(require_successful_seal(&json!({"state": state})).is_ok());
        }
    }

    #[test]
    fn parses_agent_apps_with_launch_metadata() {
        let apps = parse_agent_apps(&json!([{
            "app_id": "steam:480",
            "display_name": "Spacewar",
            "canonical_executable": "/games/spacewar",
            "desktop_entry_id": null,
            "steam_app_id": 480,
            "launcher": "steam",
            "aliases": ["Space War"],
            "icon_path": "/icons/spacewar.png"
        }]))
        .expect("valid app payload");

        assert_eq!(apps[0].app_id, "steam:480");
        assert_eq!(apps[0].steam_app_id, Some(480));
        assert_eq!(
            apps[0].canonical_executable.as_deref(),
            Some("/games/spacewar")
        );
    }

    #[test]
    fn parses_catalog_camel_case_compatibly() {
        let apps = parse_agent_catalog(&json!({
            "apps": [{
                "appId": "desktop:org.example.Game",
                "displayName": "Example Game",
                "latestBundleId": "f30a42a8-3dc9-4aea-a71c-f57f4b66bbef",
                "latestCompleteBundleId": "f30a42a8-3dc9-4aea-a71c-f57f4b66bbef",
                "latestCompleteCapturedAt": "2026-09-21T00:00:00Z",
                "bundles": []
            }]
        }))
        .expect("valid catalog payload");

        assert_eq!(apps[0].app_id, "desktop:org.example.Game");
        assert_eq!(
            apps[0].latest_bundle_id.as_deref(),
            Some("f30a42a8-3dc9-4aea-a71c-f57f4b66bbef")
        );
        assert_eq!(
            apps[0].latest_complete_bundle_id.as_deref(),
            Some("f30a42a8-3dc9-4aea-a71c-f57f4b66bbef")
        );
        assert_eq!(
            apps[0].latest_complete_captured_at.as_deref(),
            Some("2026-09-21T00:00:00Z")
        );
    }

    #[test]
    fn infers_missing_steam_metadata_from_sparse_agent_payloads() {
        let apps = parse_agent_apps(&json!([{
            "app_id": "steam:3241660",
            "display_name": "R.E.P.O.",
            "launcher": "  ",
            "aliases": ["REPO", " repo ", "R.E.P.O."]
        }]))
        .expect("valid app payload");

        assert_eq!(apps[0].steam_app_id, Some(3_241_660));
        assert_eq!(apps[0].launcher.as_deref(), Some("steam"));
        assert_eq!(apps[0].aliases, vec!["REPO".to_string()]);
    }

    #[test]
    fn infers_numeric_steam_app_ids_from_catalog_when_launcher_is_steam() {
        let apps = parse_agent_catalog(&json!({
            "apps": [{
                "appId": "3241660",
                "displayName": "R.E.P.O.",
                "launcher": "Steam",
                "latestBundleId": "f30a42a8-3dc9-4aea-a71c-f57f4b66bbef"
            }]
        }))
        .expect("valid catalog payload");

        assert_eq!(apps[0].steam_app_id, Some(3_241_660));
        assert_eq!(apps[0].launcher.as_deref(), Some("steam"));
    }
}
