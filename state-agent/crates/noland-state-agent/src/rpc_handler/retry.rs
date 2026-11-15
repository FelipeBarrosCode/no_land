//! Durable, credential-free retry identity shared by admission and reattachment.
use noland_rclone_adapter::EphemeralRcloneSession;
use noland_state_core::{OperationRecord, Result, StateError};
use serde_json::{json, Value};

use crate::operation_manager::RetryDescriptor;

const MATCHING_FIELDS: &[&str] = &[
    "app_id",
    "bundle_id",
    "mode",
    "performance_mode",
    "regenerable_caches",
    "client_operation_id",
];

fn sanitized_identity(value: &Value) -> Option<Value> {
    let mut identity = serde_json::Map::new();
    for key in ["provider", "backend_type", "remote_name", "root"] {
        identity.insert(key.into(), json!(value.get(key)?.as_str()?));
    }
    Some(Value::Object(identity))
}

pub(super) fn validate_client_storage_identity(
    params: &Value,
    session: &EphemeralRcloneSession,
) -> Result<()> {
    if let Some(identity) = params.get("client_storage_identity") {
        let expected = json!({
            "provider": session.provider,
            "backend_type": session.backend_type,
            "remote_name": session.remote_name,
            "root": session.root,
        });
        if sanitized_identity(identity).as_ref() != Some(&expected) {
            return Err(StateError::Invalid(
                "client_storage_identity does not match the storage session".into(),
            ));
        }
    }
    Ok(())
}

pub(super) fn sanitize_retry_params(params: &Value) -> Value {
    let mut sanitized = serde_json::Map::new();
    for &key in MATCHING_FIELDS {
        if let Some(value) = params.get(key).and_then(Value::as_str) {
            sanitized.insert(key.into(), json!(value));
        }
    }
    if let Some(identity) = params
        .get("client_storage_identity")
        .and_then(sanitized_identity)
    {
        sanitized.insert("client_storage_identity".into(), identity);
    }
    Value::Object(sanitized)
}

pub(super) fn matching_params(operation: &OperationRecord) -> Value {
    let mut params = sanitize_retry_params(&operation.detail_json["retry"]["params"]);
    // Canonical selection and top-level identity survive legacy retries that
    // overwrote their retry descriptor. Never copy arbitrary detail/secret data.
    for (key, value) in sanitize_retry_params(&operation.detail_json)
        .as_object()
        .unwrap()
    {
        params[key] = value.clone();
    }
    params
}

pub(super) fn retry_descriptor_for(operation: &OperationRecord) -> Result<RetryDescriptor> {
    let mut params = matching_params(operation);
    let method = match operation.kind.as_str() {
        "backup" => "StartBackup",
        "restore" => "StartRestore",
        "seal" => "StartSeal",
        kind => {
            return Err(StateError::Invalid(format!(
                "operation {} of kind {kind} cannot be reconstructed for retry",
                operation.operation_id
            )))
        }
    };
    let fields = params.as_object_mut().unwrap();
    let app_id = operation
        .app_id
        .as_ref()
        .map(|id| id.as_str())
        .unwrap_or("*");
    fields.entry("app_id").or_insert_with(|| json!(app_id));
    fields
        .entry("mode")
        .or_insert_with(|| json!("personal_state"));
    if operation.kind == "backup" {
        fields
            .entry("performance_mode")
            .or_insert_with(|| json!("balanced"));
        fields
            .entry("regenerable_caches")
            .or_insert_with(|| json!("include"));
    }
    if operation.kind == "restore" && (!fields.contains_key("bundle_id") || fields["app_id"] == "*")
    {
        return Err(StateError::Invalid(
            "persisted restore selection is missing".into(),
        ));
    }
    if let Some(raw) = fields.get("client_operation_id").and_then(Value::as_str) {
        if uuid::Uuid::parse_str(raw).ok() != Some(operation.operation_id) {
            return Err(StateError::Invalid(
                "persisted client operation ID does not match".into(),
            ));
        }
    }
    Ok(RetryDescriptor {
        method: method.into(),
        params,
    })
}

fn same_parameter(key: &str, left: &Value, right: &Value) -> bool {
    match key {
        "mode" => {
            let normalize = |value: &Value| {
                value.as_str().map(|s| {
                    if s == "complete" {
                        "complete_application"
                    } else {
                        s
                    }
                    .to_owned()
                })
            };
            normalize(left).is_some() && normalize(left) == normalize(right)
        }
        "app_id" => {
            left == right
                || matches!(
                    (left.as_str(), right.as_str()),
                    (Some("*"), Some("_all")) | (Some("_all"), Some("*"))
                )
        }
        "client_storage_identity" => {
            sanitized_identity(left).is_some()
                && sanitized_identity(left) == sanitized_identity(right)
        }
        _ => left == right,
    }
}

pub(super) fn overlay_retry_secrets(stored: &mut Value, incoming: &Value) -> Result<()> {
    let fields = stored
        .as_object_mut()
        .ok_or_else(|| StateError::Invalid("stored retry parameters are not an object".into()))?;
    let incoming = incoming
        .as_object()
        .ok_or_else(|| StateError::Invalid("retry parameters are not an object".into()))?;
    // Reject attempts to change selection/identity instead of silently ignoring
    // them. Only fresh credentials may replace the original request's values.
    for (key, value) in incoming {
        if matches!(key.as_str(), "operation_id" | "session" | "master_key_hex") {
            continue;
        }
        if !fields
            .get(key)
            .is_some_and(|original| same_parameter(key, original, value))
        {
            return Err(StateError::Invalid(format!("retry cannot override {key}")));
        }
    }
    for key in ["session", "master_key_hex"] {
        if let Some(value) = incoming.get(key) {
            fields.insert(key.into(), value.clone());
        }
    }
    Ok(())
}

pub(super) fn validate_retry_selection(
    operation: &OperationRecord,
    selection: &Value,
) -> Result<()> {
    let original = retry_descriptor_for(operation)?.params;
    for (key, value) in selection
        .as_object()
        .ok_or_else(|| StateError::Invalid("invalid submission selection".into()))?
    {
        if !original
            .get(key)
            .is_some_and(|saved| same_parameter(key, saved, value))
        {
            return Err(StateError::Invalid(format!("retry cannot override {key}")));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "retry_tests.rs"]
mod tests;
