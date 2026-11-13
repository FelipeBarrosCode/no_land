//! Read-only monitoring of an already submitted operation. Reconnection must
//! never submit/retry work: the remote agent owns execution and its terminal state.

use std::future::Future;
use std::time::Duration;

use serde_json::{json, Value};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum MonitorFailure {
    Terminal { state: String, message: String },
    TimedOut { message: String },
}

impl MonitorFailure {
    pub(super) fn keep_active_handle(&self) -> bool {
        matches!(self, Self::TimedOut { .. })
    }
}

#[derive(Clone, Copy)]
struct PollPolicy {
    interval: Duration,
    idle_max: Duration,
    fetch_timeout: Duration,
    reconnect_initial: Duration,
    reconnect_max: Duration,
}

const POLL_POLICY: PollPolicy = PollPolicy {
    interval: Duration::from_secs(1),
    idle_max: Duration::from_secs(8),
    fetch_timeout: Duration::from_secs(20),
    reconnect_initial: Duration::from_secs(1),
    reconnect_max: Duration::from_secs(15),
};

pub(super) async fn monitor_operation<F, Fut, E, Emit>(
    operation_id: &str,
    timeout: Duration,
    mut initial_status: Option<Value>,
    mut fetch: F,
    emit: Emit,
) -> Result<Value, MonitorFailure>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<Value, E>>,
    Emit: FnMut(&Value),
{
    monitor_with_policy(
        operation_id,
        timeout,
        POLL_POLICY,
        |id| {
            let initial = initial_status.take();
            let pending = initial.is_none().then(|| fetch(id));
            async move {
                match initial {
                    Some(status) => Ok(status),
                    None => pending.expect("uncached status fetch").await,
                }
            }
        },
        emit,
    )
    .await
}

async fn monitor_with_policy<F, Fut, E, Emit>(
    operation_id: &str,
    timeout: Duration,
    policy: PollPolicy,
    mut fetch: F,
    mut emit: Emit,
) -> Result<Value, MonitorFailure>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<Value, E>>,
    Emit: FnMut(&Value),
{
    let mut last_status = None;
    let mut failures = 0u32;
    let mut idle_polls = 0u32;
    let result = tokio::time::timeout(timeout, async {
        loop {
            // Transport/RPC/decoding errors are absence of an authoritative
            // status, not FAILED. Even a mismatched response must not terminate
            // monitoring of this operation (or be mistaken for its completion).
            let mut status = match tokio::time::timeout(policy.fetch_timeout, fetch(operation_id.to_owned())).await {
                Ok(Ok(status)) if valid_status(&status, operation_id) => status,
                _ => {
                    failures = failures.saturating_add(1);
                    let delay = reconnect_delay(policy, failures);
                    emit(&connection_status(operation_id, last_status.as_ref(), "RECONNECTING", format!(
                        "Reconnecting to operation {operation_id} (attempt {failures}); checking the same operation again in {}s. Remote work may still be running.",
                        delay.as_secs()
                    )));
                    tokio::time::sleep(delay).await;
                    continue;
                }
            };
            retain_transfer_progress(&mut status, last_status.as_ref());
            let changed = last_status.as_ref().is_none_or(|last| meaningful_status(last) != meaningful_status(&status));
            if changed || failures > 0 {
                emit(&status);
                idle_polls = 0;
            } else {
                idle_polls = idle_polls.saturating_add(1);
            }
            failures = 0;
            let state = status["state"].as_str().expect("validated status state");
            match state {
                "COMPLETED" | "SEALED" | "ROLLED_BACK" => return Ok(status),
                "FAILED" | "CANCELLED" | "INTERRUPTED" => {
                    let detail = status.get("last_error").and_then(Value::as_str).unwrap_or(state);
                    return Err(MonitorFailure::Terminal {
                        state: state.into(),
                        message: format!("state-agent operation {operation_id}: {detail}"),
                    });
                }
                _ => {
                    // READY_TO_LAUNCH is a milestone, never a terminal result.
                    last_status = Some(status);
                    tokio::time::sleep(idle_delay(policy, idle_polls)).await;
                }
            }
        }
    }).await;
    match result {
        Ok(result) => result,
        Err(_) => {
            let message = format!(
                "Monitoring timed out for state-agent operation {operation_id}. Remote work may still be running; resume monitoring or cancel this same operation ID."
            );
            emit(&connection_status(
                operation_id,
                last_status.as_ref(),
                "MONITORING_PAUSED",
                message.clone(),
            ));
            Err(MonitorFailure::TimedOut { message })
        }
    }
}

fn reconnect_delay(policy: PollPolicy, failures: u32) -> Duration {
    policy
        .reconnect_initial
        .saturating_mul(1u32 << failures.saturating_sub(1).min(16))
        .min(policy.reconnect_max)
}

fn idle_delay(policy: PollPolicy, idle_polls: u32) -> Duration {
    policy
        .interval
        .saturating_mul(1u32 << idle_polls.min(16))
        .min(policy.idle_max)
}

// Compare only observable progress, not freshly sampled timestamps, elapsed
// timings or the potentially large retry/result documents returned by old agents.
fn meaningful_status(status: &Value) -> Value {
    let progress = &status["progress"];
    let detail = &progress["detail_json"];
    let visible_progress = json!({
        "phase": progress.get("phase"), "message": progress.get("message"),
        "completed_units": progress.get("completed_units"), "total_units": progress.get("total_units"),
        "unit": progress.get("unit"), "total_packs": detail.get("total_packs"),
        "completed_pack_bytes": detail.get("completed_pack_bytes"), "bytes_transferred": detail.get("bytes_transferred"),
        "total_transfer_bytes": detail.get("total_transfer_bytes"),
        "published_bytes": detail.pointer("/pipeline/published_bytes"), "total_bytes": detail.pointer("/pipeline/total_bytes"),
        "ready_to_launch_reached": detail.get("ready_to_launch_reached"), "milestones": detail.get("milestones"),
        "captured_bytes": detail.pointer("/streaming_pipeline/durable_bytes"),
        "prepared_bytes": detail.pointer("/streaming_pipeline/prepared_bytes"),
        "active_readers": detail.pointer("/streaming_pipeline/active_readers"),
        "active_codecs": detail.pointer("/streaming_pipeline/active_codecs"),
        "inflight_uploads": detail.pointer("/streaming_pipeline/inflight_uploads"),
        // This snapshot changes on confirmed requests/phases, not on each poll.
        "deletion": detail.get("deletion"),
    });
    json!({
        "operation_id": status.get("operation_id"), "state": status.get("state"),
        "running": status.get("running"), "cancel_requested": status.get("cancel_requested"),
        "last_error": status.get("last_error"), "progress": visible_progress,
        "pack_transfer": status.get("pack_transfer"),
    })
}

// Commit/verification phases replace the progress document. Keep established
// transfer counters visible without turning logical reuse into network traffic.
fn retain_transfer_progress(status: &mut Value, previous: Option<&Value>) {
    let Some(previous) = previous else { return };
    if matches!(status["state"].as_str(), Some("QUEUED" | "REQUESTED")) {
        return;
    }
    if status.get("pack_transfer").is_none_or(Value::is_null) {
        if let Some(transfer) = previous.get("pack_transfer") {
            status["pack_transfer"] = transfer.clone();
        }
    }
    let Some(old_detail) = previous
        .pointer("/progress/detail_json")
        .and_then(Value::as_object)
    else {
        return;
    };
    if !status.get("progress").is_some_and(Value::is_object) {
        status["progress"] = json!({});
    }
    if !status["progress"]
        .get("detail_json")
        .is_some_and(Value::is_object)
    {
        status["progress"]["detail_json"] = json!({});
    }
    let detail = status["progress"]["detail_json"]
        .as_object_mut()
        .expect("progress detail object");
    for key in [
        "pipeline",
        "streaming_pipeline",
        "total_packs",
        "completed_pack_bytes",
        "bytes_transferred",
        "total_transfer_bytes",
    ] {
        if detail.get(key).is_none_or(Value::is_null) {
            if let Some(value) = old_detail.get(key) {
                detail.insert(key.into(), value.clone());
            }
        }
    }
}

fn valid_status(status: &Value, operation_id: &str) -> bool {
    status.get("operation_id").and_then(Value::as_str) == Some(operation_id)
        && status
            .get("state")
            .and_then(Value::as_str)
            .is_some_and(|state| !state.is_empty())
}

fn connection_status(
    operation_id: &str,
    last_status: Option<&Value>,
    state: &str,
    message: String,
) -> Value {
    let mut status = last_status.cloned().unwrap_or_else(|| json!({}));
    status["operation_id"] = operation_id.into();
    status["state"] = state.into();
    // Keep cancellation reachable while the execution state is unknown.
    status["running"] = true.into();
    if !status.get("progress").is_some_and(Value::is_object) {
        status["progress"] = json!({});
    }
    status["progress"]["phase"] = "reconnecting".into();
    status["progress"]["message"] = message.into();
    status
}

pub(super) struct OperationMatch<'a> {
    pub kind: &'a str,
    pub app_id: &'a str,
    pub bundle_id: Option<&'a str>,
    pub mode: &'a str,
    pub storage_identity: &'a Value,
    pub regenerable_caches: &'a str,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ExistingOperation {
    Running(String),
    /// A retained timeout handle may have completed while the client was away.
    /// Observe its result instead of submitting the same request again.
    Tracked(String),
    Interrupted(String),
    /// A legacy operation has no provider binding: don't risk duplicate work
    /// or attach this request to a different repository.
    UnboundRunning(String),
}

pub(super) fn select_existing_operation(
    operations: &[Value],
    request: &OperationMatch<'_>,
    preferred_id: Option<&str>,
) -> Option<ExistingOperation> {
    let mut running = None;
    let mut interrupted = None;
    let mut unbound = None;
    for operation in operations {
        if operation.get("kind").and_then(Value::as_str) != Some(request.kind) {
            continue;
        }
        let Some(id) = operation
            .get("operation_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        else {
            continue;
        };
        let detail = &operation["detail_json"];
        let retry = &detail["retry"]["params"];
        let parameter = |name: &str| retry.get(name).or_else(|| detail.get(name));
        let app = parameter("app_id")
            .and_then(Value::as_str)
            .or_else(|| operation.get("app_id").and_then(Value::as_str));
        let app_matches = match request.app_id {
            "*" | "_all" => app.is_none() || matches!(app, Some("*" | "_all")),
            expected => app == Some(expected),
        };
        if !app_matches
            || parameter("mode")
                .and_then(Value::as_str)
                .map(normalize_mode)
                != Some(normalize_mode(request.mode))
            || request.bundle_id.is_some_and(|expected| {
                parameter("bundle_id").and_then(Value::as_str) != Some(expected)
            })
            || (request.kind == "backup"
                && parameter("regenerable_caches")
                    .and_then(Value::as_str)
                    .unwrap_or("include")
                    != request.regenerable_caches)
        {
            continue;
        }
        let state = operation
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("UNKNOWN");
        let is_running = operation.get("running").and_then(Value::as_bool) == Some(true)
            && !matches!(
                state,
                "COMPLETED" | "SEALED" | "ROLLED_BACK" | "FAILED" | "CANCELLED" | "INTERRUPTED"
            );
        match parameter("client_storage_identity") {
            Some(identity) if identity == request.storage_identity => {}
            None if is_running => {
                unbound.get_or_insert_with(|| ExistingOperation::UnboundRunning(id.into()));
                continue;
            }
            _ => continue,
        }
        if Some(id) == preferred_id
            && matches!(
                state,
                "COMPLETED" | "SEALED" | "ROLLED_BACK" | "FAILED" | "CANCELLED"
            )
        {
            return Some(ExistingOperation::Tracked(id.into()));
        }
        if is_running {
            if Some(id) == preferred_id {
                return Some(ExistingOperation::Running(id.into()));
            }
            running.get_or_insert_with(|| ExistingOperation::Running(id.into()));
        } else if state == "INTERRUPTED" {
            interrupted.get_or_insert_with(|| ExistingOperation::Interrupted(id.into()));
        }
    }
    // Never retry an older interrupted operation while an identical one runs.
    running.or(unbound).or(interrupted)
}

fn normalize_mode(mode: &str) -> &str {
    match mode {
        "complete" => "complete_application",
        mode => mode,
    }
}

#[cfg(test)]
#[path = "operation_monitor_tests.rs"]
mod tests;
