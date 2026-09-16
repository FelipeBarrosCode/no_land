use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use noland_rclone_adapter::EphemeralRcloneSession;
use noland_rpc::{RpcRequest, RpcResponse, VerifyBackupCommitResult};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::time::timeout;
use uuid::Uuid;

use crate::{AgentError, Result};

const STATE_AGENT_RPC_TIMEOUT: Duration = Duration::from_secs(30);
const XPROP_TIMEOUT: Duration = Duration::from_millis(500);
const MAX_RPC_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ActiveAppSession {
    #[serde(alias = "sessionId")]
    pub session_id: String,
    #[serde(alias = "appId")]
    pub app_id: String,
    #[serde(default)]
    pub pids: Vec<u32>,
    #[serde(default, alias = "rootPid")]
    pub root_pid: Option<u32>,
}

#[derive(Clone)]
pub struct BackupRequest {
    pub app_id: String,
    pub operation_id: Uuid,
    pub session: EphemeralRcloneSession,
    pub master_key_hex: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationStatus {
    pub state: String,
    pub detail_json: Value,
}

#[derive(Clone)]
pub struct VerifyRequest {
    pub app_id: String,
    pub bundle_id: String,
    pub commit_id: String,
    pub session: EphemeralRcloneSession,
    pub master_key_hex: String,
}

#[async_trait]
pub trait StateAgentClient: Send + Sync {
    async fn get_active_app_sessions(&self) -> Result<Vec<ActiveAppSession>>;
    async fn resolve_process_to_app(&self, pid: u32) -> Result<Option<String>>;
    async fn start_backup(&self, request: BackupRequest) -> Result<String>;
    async fn get_operation_status(&self, operation_id: &str) -> Result<OperationStatus>;
    async fn verify_backup_commit(&self, request: VerifyRequest) -> Result<bool>;
}

pub struct UnixStateAgentClient {
    socket_path: PathBuf,
}

impl UnixStateAgentClient {
    pub fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let request_id = Uuid::new_v4().to_string();
        let request = RpcRequest {
            id: request_id.clone(),
            method: method.to_string(),
            params,
        };
        let operation = async {
            let mut stream = UnixStream::connect(&self.socket_path).await?;
            let peer = stream.peer_cred()?;
            if peer.uid() != 0 {
                return Err(AgentError::new(
                    "state-agent Unix socket peer is not the trusted root service",
                ));
            }
            let mut encoded = serde_json::to_vec(&request)?;
            encoded.push(b'\n');
            stream.write_all(&encoded).await?;
            stream.shutdown().await?;

            let mut response_line = String::new();
            let reader = BufReader::new(stream);
            let mut limited = reader.take(MAX_RPC_RESPONSE_BYTES);
            let bytes = limited.read_line(&mut response_line).await?;
            if bytes == 0 || !response_line.ends_with('\n') {
                return Err(AgentError::new(
                    "state-agent returned an incomplete response",
                ));
            }
            let response: RpcResponse = serde_json::from_str(&response_line)?;
            if response.id != request_id {
                return Err(AgentError::new("state-agent response ID mismatch"));
            }
            if response.error.is_some() {
                return Err(AgentError::new("state-agent RPC reported an error"));
            }
            response
                .result
                .ok_or_else(|| AgentError::new("state-agent response has no result"))
        };
        timeout(STATE_AGENT_RPC_TIMEOUT, operation)
            .await
            .map_err(|_| AgentError::new("state-agent RPC timed out"))?
    }
}

#[async_trait]
impl StateAgentClient for UnixStateAgentClient {
    async fn get_active_app_sessions(&self) -> Result<Vec<ActiveAppSession>> {
        let value = self.call("GetActiveAppSessions", json!({})).await?;
        let sessions = value.get("sessions").cloned().unwrap_or(value);
        serde_json::from_value(sessions).map_err(Into::into)
    }

    async fn resolve_process_to_app(&self, pid: u32) -> Result<Option<String>> {
        let value = self
            .call("ResolveProcessToApp", json!({"pid": pid}))
            .await?;
        if value.is_null() {
            return Ok(None);
        }
        if let Some(app_id) = value.as_str() {
            return Ok(Some(app_id.to_string()));
        }
        Ok(value
            .get("app_id")
            .or_else(|| value.get("appId"))
            .and_then(Value::as_str)
            .map(str::to_owned))
    }

    async fn start_backup(&self, request: BackupRequest) -> Result<String> {
        let value = self
            .call(
                "StartBackup",
                json!({
                    "app_id": request.app_id,
                    "mode": "personal_state",
                    "performance_mode": "balanced",
                    "session": request.session,
                    "master_key_hex": request.master_key_hex,
                    "operation_id": request.operation_id,
                }),
            )
            .await?;
        value
            .get("operation_id")
            .or_else(|| value.get("operationId"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| AgentError::new("StartBackup response has no operation ID"))
    }

    async fn get_operation_status(&self, operation_id: &str) -> Result<OperationStatus> {
        let value = self
            .call("GetOperationStatus", json!({"operation_id": operation_id}))
            .await?;
        let state = value
            .get("state")
            .and_then(Value::as_str)
            .ok_or_else(|| AgentError::new("operation status has no state"))?
            .to_ascii_uppercase();
        let detail_json = value
            .get("detail_json")
            .or_else(|| value.get("detailJson"))
            .cloned()
            .unwrap_or_else(|| json!({}));
        Ok(OperationStatus { state, detail_json })
    }

    async fn verify_backup_commit(&self, request: VerifyRequest) -> Result<bool> {
        let expected_app = request.app_id.clone();
        let expected_bundle = request.bundle_id.clone();
        let expected_commit = request.commit_id.clone();
        let value = self
            .call(
                "VerifyBackupCommit",
                json!({
                    "app_id": request.app_id,
                    "bundle_id": request.bundle_id,
                    "commit_id": request.commit_id,
                    "session": request.session,
                    "master_key_hex": request.master_key_hex,
                }),
            )
            .await?;
        verification_result_matches(value, &expected_app, &expected_bundle, &expected_commit)
    }
}

fn verification_result_matches(
    value: Value,
    expected_app: &str,
    expected_bundle: &str,
    expected_commit: &str,
) -> Result<bool> {
    let result: VerifyBackupCommitResult = serde_json::from_value(value)
        .map_err(|_| AgentError::new("state-agent returned an invalid verification result"))?;
    Ok(result.verified
        && result.app_id.to_string() == expected_app
        && result.bundle_id.to_string() == expected_bundle
        && result.commit_id.to_string() == expected_commit)
}

#[async_trait]
pub trait ForegroundPidBackend: Send + Sync {
    async fn foreground_pid(&self) -> Option<u32>;
}

pub struct XpropForegroundPidBackend;

#[async_trait]
impl ForegroundPidBackend for XpropForegroundPidBackend {
    async fn foreground_pid(&self) -> Option<u32> {
        let root = run_xprop(&["-root", "_NET_ACTIVE_WINDOW"]).await?;
        let window_id = root.split_whitespace().last()?;
        if window_id == "0x0" || window_id == "0" {
            return None;
        }
        let pid = run_xprop(&["-id", window_id, "_NET_WM_PID"]).await?;
        pid.split_whitespace().last()?.parse().ok()
    }
}

async fn run_xprop(arguments: &[&str]) -> Option<String> {
    let mut command = Command::new("xprop");
    command.args(arguments).kill_on_drop(true);
    let output = timeout(XPROP_TIMEOUT, command.output()).await.ok()?.ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

pub fn socket_path_is_unix(path: &Path) -> bool {
    path.is_absolute()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verification_requires_every_exact_identity_field() {
        let bundle_id = Uuid::new_v4();
        let commit_id = Uuid::new_v4();
        let exact = json!({
            "verified": true,
            "app_id": "steam:42",
            "bundle_id": bundle_id,
            "commit_id": commit_id,
        });
        assert!(verification_result_matches(
            exact,
            "steam:42",
            &bundle_id.to_string(),
            &commit_id.to_string(),
        )
        .unwrap());

        let incomplete = json!({"verified": true});
        assert!(verification_result_matches(
            incomplete,
            "steam:42",
            &bundle_id.to_string(),
            &commit_id.to_string(),
        )
        .is_err());

        let wrong_app = json!({
            "verified": true,
            "app_id": "steam:99",
            "bundle_id": bundle_id,
            "commit_id": commit_id,
        });
        assert!(!verification_result_matches(
            wrong_app,
            "steam:42",
            &bundle_id.to_string(),
            &commit_id.to_string(),
        )
        .unwrap());
    }
}
