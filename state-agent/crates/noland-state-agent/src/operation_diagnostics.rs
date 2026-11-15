//! Retain small, credential-free diagnostics before progress is cleared/replaced.
//! Copy the I/O object generically so new scheduler counters survive completion.
use noland_state_core::{OperationProgress, OperationRecord, Result};

use crate::StateAgent;

const DETAIL_KEYS: &[&str] = &[
    "pipeline",
    "streaming_pipeline",
    "transfer_plan",
    "catalog",
    "remote_metadata",
    "remote_index_cache_us",
    "post_commit_cache_us",
    "complete_published_files",
    "complete_reused_files",
    "restored_all_manifest_files",
    "terminal_pack_cache_cleanup",
    "repository_admission",
    "completion",
];

pub(crate) fn retain(agent: &StateAgent, operation: &mut OperationRecord) -> Result<()> {
    let Some(progress) = agent.db.get_operation_progress(operation.operation_id)? else {
        return Ok(());
    };
    if !operation.detail_json.is_object() {
        operation.detail_json = serde_json::json!({});
    }
    for &key in DETAIL_KEYS {
        if let Some(value) = progress.detail_json.get(key) {
            operation.detail_json[key] = value.clone();
        }
    }
    Ok(())
}

pub(crate) fn begin_retry(operation: &mut OperationRecord) {
    if let Some(fields) = operation.detail_json.as_object_mut() {
        let previous = DETAIL_KEYS
            .iter()
            .filter_map(|&key| fields.remove(key).map(|value| (key.into(), value)))
            .collect::<serde_json::Map<_, _>>();
        if !previous.is_empty() {
            fields.insert("previous_attempt_diagnostics".into(), previous.into());
        }
    }
}

/// Ordinary result/progress data may be cleared on completion, but a cleanup
/// warning must remain visible to the existing lightweight progress projection.
pub(crate) fn clear_completed_progress(
    agent: &StateAgent,
    operation: &OperationRecord,
) -> Result<()> {
    let admission = &operation.detail_json["repository_admission"];
    if admission["cleanup_required"] != true {
        return agent
            .db
            .set_operation_progress(operation.operation_id, None);
    }
    let mut progress = agent
        .db
        .get_operation_progress(operation.operation_id)?
        .unwrap_or_else(|| OperationProgress::new(&operation.state, 0));
    progress.phase = operation.state.clone();
    progress.updated_at = chrono::Utc::now();
    progress.message = Some("Operation completed; repository claim cleanup is required".into());
    if !progress.detail_json.is_object() {
        progress.detail_json = serde_json::json!({});
    }
    progress.detail_json["repository_admission"] = admission.clone();
    agent
        .db
        .set_operation_progress(operation.operation_id, Some(&progress))
}

/// Publication is durable, but success is not yet safe to advertise: a client
/// may destroy its PC as soon as it observes a terminal operation state.
pub(crate) fn prepare_completion(
    agent: &StateAgent,
    id: uuid::Uuid,
    finalizing_state: &str,
    terminal_state: &str,
) -> Result<()> {
    let Some(mut operation) = agent.db.get_operation(id)? else {
        return Ok(());
    };
    retain(agent, &mut operation)?;
    if !operation.detail_json.is_object() {
        operation.detail_json = serde_json::json!({});
    }
    operation.state = finalizing_state.into();
    operation.updated_at = chrono::Utc::now();
    operation.detail_json["completion"] = serde_json::json!({
        "durable": true,
        "status": "FINALIZING_REPOSITORY",
        "terminal_state": terminal_state,
    });
    agent.db.upsert_operation(&operation)?;
    let mut progress = agent
        .db
        .get_operation_progress(id)?
        .unwrap_or_else(|| OperationProgress::new(finalizing_state, 0));
    progress.phase = finalizing_state.into();
    progress.message = Some("Data committed; finalizing repository admission".into());
    progress.updated_at = operation.updated_at;
    if !progress.detail_json.is_object() {
        progress.detail_json = serde_json::json!({});
    }
    progress.detail_json["completion"] = operation.detail_json["completion"].clone();
    agent.db.set_operation_progress(id, Some(&progress))
}

pub(crate) fn complete_after_repository_release(agent: &StateAgent, id: uuid::Uuid) -> Result<()> {
    let Some(mut operation) = agent.db.get_operation(id)? else {
        return Ok(());
    };
    let terminal = match operation.kind.as_str() {
        "backup" | "restore" => "COMPLETED",
        "seal" => "SEALED",
        _ => {
            return Err(noland_state_core::StateError::Invalid(
                "invalid completion kind".into(),
            ))
        }
    };
    if operation.detail_json["completion"]["durable"] != true
        || operation.detail_json["completion"]["terminal_state"] != terminal
        || !matches!(
            operation.detail_json["repository_admission"]["status"].as_str(),
            Some("RELEASED" | "CLEANUP_REQUIRED")
        )
    {
        return Err(noland_state_core::StateError::Storage(
            "OPERATION_FINALIZATION_REQUIRED: durable data awaits confirmed repository finalization diagnostics".into(),
        ));
    }
    let cleanup = operation.detail_json["repository_admission"]["cleanup_required"] == true;
    let mut progress = agent
        .db
        .get_operation_progress(id)?
        .unwrap_or_else(|| OperationProgress::new(terminal, 0));
    progress.phase = terminal.into();
    progress.completed_units = progress.total_units.unwrap_or(progress.completed_units);
    progress.updated_at = chrono::Utc::now();
    progress.message = Some(
        if cleanup {
            "Operation completed; repository claim cleanup is required"
        } else if operation.kind == "restore" {
            "Restore completed"
        } else {
            "Operation completed"
        }
        .into(),
    );
    if !progress.detail_json.is_object() {
        progress.detail_json = serde_json::json!({});
    }
    operation.detail_json["completion"]["status"] = serde_json::json!("FINALIZED");
    progress.detail_json["completion"] = operation.detail_json["completion"].clone();
    if operation.kind == "restore" {
        progress.detail_json["milestones"] = serde_json::json!(["READY_TO_LAUNCH", "COMPLETE"]);
    } else if operation.kind == "backup" {
        if let Some(pipeline) = progress
            .detail_json
            .get_mut("streaming_pipeline")
            .and_then(serde_json::Value::as_object_mut)
        {
            pipeline.insert("waiting_on".into(), serde_json::json!("complete"));
        }
    }
    agent.db.set_operation_progress(id, Some(&progress))?;
    // Archive the final report without copying an older completion marker back.
    retain(agent, &mut operation)?;
    operation.state = terminal.into();
    operation.last_error = None;
    operation.updated_at = chrono::Utc::now();
    if operation.kind != "restore" {
        clear_completed_progress(agent, &operation)?;
    }
    // Terminal publication is last, after the release attempt and safe details.
    agent.db.upsert_operation(&operation)
}
