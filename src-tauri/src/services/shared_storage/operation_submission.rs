//! API 19 admission: retain the caller's operation ID before crossing the
//! transport boundary. Missing replies never authorize a fresh submission ID.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Mutex;
use std::time::Duration;

#[derive(Debug)]
pub(super) enum SubmissionAttemptError {
    Unavailable,
    Rejected(String),
}

#[derive(Debug)]
pub(super) enum SubmissionFailure {
    Unconfirmed(String),
    Rejected(String),
    Incompatible(String),
}

#[derive(Debug)]
pub(super) struct SubmissionReceipt {
    pub operation_id: String,
    pub status: Option<Value>,
}

impl SubmissionReceipt {
    pub(super) fn attached(operation_id: String) -> Self {
        Self {
            operation_id,
            status: None,
        }
    }
}

/// Retain every unresolved instance/operation binding until its exact ID is
/// forgotten. Only digests are retained, never credentials or request bodies.
/// A process restart intentionally loses authority to replay an unobserved
/// request; remote matching then fails closed. There is no eviction/expiry.
pub(super) struct SubmissionMemory(Mutex<BTreeMap<(u64, String), [u8; 32]>>);

impl SubmissionMemory {
    pub(super) const fn new() -> Self {
        Self(Mutex::new(BTreeMap::new()))
    }

    /// An ID cannot be rebound to changed selection/provider/vault inputs.
    /// False means the original binding remains intact and submission must stop.
    pub(super) fn remember(
        &self,
        instance_id: u64,
        operation_id: &str,
        fingerprint: [u8; 32],
    ) -> bool {
        let mut saved = self.0.lock().unwrap_or_else(|error| error.into_inner());
        let retained = saved
            .entry((instance_id, operation_id.to_owned()))
            .or_insert(fingerprint);
        *retained == fingerprint
    }

    pub(super) fn lookup(&self, instance_id: u64, fingerprint: &[u8; 32]) -> Option<String> {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .find_map(|((instance, id), saved)| {
                (*instance == instance_id && saved == fingerprint).then(|| id.clone())
            })
    }

    pub(super) fn matches(
        &self,
        instance_id: u64,
        operation_id: &str,
        fingerprint: &[u8; 32],
    ) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&(instance_id, operation_id.to_owned()))
            == Some(fingerprint)
    }

    pub(super) fn forget(&self, instance_id: u64, operation_id: &str) {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&(instance_id, operation_id.to_owned()));
    }

    pub(super) fn has_conflicting_binding(
        &self,
        instance_id: u64,
        operation_id: &str,
        fingerprint: &[u8; 32],
    ) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&(instance_id, operation_id.to_owned()))
            .is_some_and(|saved| saved != fingerprint)
    }
}

pub(super) async fn clear_rejected_submission(
    result: &Result<SubmissionReceipt, SubmissionFailure>,
    memory: &SubmissionMemory,
    instance_id: u64,
    operation_id: &str,
    clear: impl Future<Output = ()>,
) -> bool {
    if !matches!(result, Err(SubmissionFailure::Rejected(_))) {
        return false;
    }
    memory.forget(instance_id, operation_id);
    clear.await;
    true
}

pub(super) fn request_fingerprint(kind: &str, params: &Value) -> [u8; 32] {
    let text = |key: &str, default: &str| {
        params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or(default)
            .to_owned()
    };
    let app = match text("app_id", "*").as_str() {
        "_all" => "*".into(),
        app => app.to_owned(),
    };
    let mode = match text("mode", "personal_state").as_str() {
        "complete" => "complete_application".into(),
        mode => mode.to_owned(),
    };
    let session = &params["session"];
    // Match immutable selection/provider/vault inputs. Short-lived credentials,
    // expiry and session IDs may legitimately change when the UI resumes.
    let immutable = json!({
        "domain": "noland-client-submission-v1", "kind": kind, "app_id": app,
        "mode": mode, "bundle_id": params.get("bundle_id"),
        "performance_mode": text("performance_mode", "automatic"),
        "regenerable_caches": text("regenerable_caches", "include"),
        "provider": session.get("provider"), "backend_type": session.get("backend_type"),
        "remote_name": session.get("remote_name"), "root": session.get("root"),
        "vault": text("master_key_hex", "").to_ascii_lowercase(),
    });
    Sha256::digest(serde_json::to_vec(&immutable).expect("JSON values serialize")).into()
}

#[derive(Clone, Copy)]
struct RetryPolicy {
    attempts: u32,
    timeout: Duration,
    initial: Duration,
    maximum: Duration,
}

const RETRY_POLICY: RetryPolicy = RetryPolicy {
    attempts: 5,
    timeout: Duration::from_secs(120),
    initial: Duration::from_secs(1),
    maximum: Duration::from_secs(15),
};

pub(super) async fn submit_with_id<Remember, RememberFuture, Send, SendFuture, Emit>(
    operation_id: String,
    remember: Remember,
    send: Send,
    emit: Emit,
) -> Result<SubmissionReceipt, SubmissionFailure>
where
    Remember: FnOnce(String) -> RememberFuture,
    RememberFuture: Future<Output = ()>,
    Send: FnMut(String) -> SendFuture,
    SendFuture: Future<Output = Result<Value, SubmissionAttemptError>>,
    Emit: FnMut(&Value),
{
    submit_with_policy(operation_id, RETRY_POLICY, remember, send, emit).await
}

async fn submit_with_policy<Remember, RememberFuture, Send, SendFuture, Emit>(
    operation_id: String,
    policy: RetryPolicy,
    remember: Remember,
    mut send: Send,
    mut emit: Emit,
) -> Result<SubmissionReceipt, SubmissionFailure>
where
    Remember: FnOnce(String) -> RememberFuture,
    RememberFuture: Future<Output = ()>,
    Send: FnMut(String) -> SendFuture,
    SendFuture: Future<Output = Result<Value, SubmissionAttemptError>>,
    Emit: FnMut(&Value),
{
    remember(operation_id.clone()).await;
    let admitted = tokio::time::timeout(policy.timeout, async {
        for attempt in 1..=policy.attempts {
            match send(operation_id.clone()).await {
                Ok(response) => match response.get("operation_id").and_then(Value::as_str) {
                    Some(id) if id == operation_id => return Ok(Some(SubmissionReceipt {
                        operation_id: operation_id.clone(),
                        status: response.get("state").and_then(Value::as_str).map(|_| response.clone()),
                    })),
                    Some(_) => return Err(SubmissionFailure::Incompatible(format!(
                        "state-agent returned a different operation ID; retain and resume operation {operation_id}. API 19 is required."
                    ))),
                    None => {} // Ambiguous reply: replay only this same ID.
                },
                Err(SubmissionAttemptError::Rejected(error)) => return Err(SubmissionFailure::Rejected(format!(
                    "Submission for operation {operation_id} was rejected: {error}"
                ))),
                Err(SubmissionAttemptError::Unavailable) => {}
            }
            if attempt == policy.attempts { break; }
            let delay = policy.initial.saturating_mul(1u32 << attempt.saturating_sub(1).min(16)).min(policy.maximum);
            emit(&json!({
                "operation_id": operation_id, "state": "RECONNECTING", "running": true,
                "progress": {"phase": "reconnecting", "message": format!(
                    "Reconnecting to submit/resume operation {operation_id}; retrying the same request ID (attempt {}).", attempt + 1
                )},
            }));
            tokio::time::sleep(delay).await;
        }
        Ok(None)
    }).await;
    match admitted {
        Ok(Ok(Some(receipt))) => return Ok(receipt),
        Ok(Err(error)) => return Err(error),
        _ => {}
    }
    let message = format!(
        "Could not confirm submission of operation {operation_id}. Remote work may already be running; resume this same request and operation ID."
    );
    emit(&json!({
        "operation_id": operation_id, "state": "MONITORING_PAUSED", "running": true,
        "progress": {"phase": "reconnecting", "message": message},
    }));
    Err(SubmissionFailure::Unconfirmed(message))
}

#[cfg(test)]
#[path = "operation_submission_tests.rs"]
mod tests;
