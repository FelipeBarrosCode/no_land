//! Local Unix-socket JSON-RPC. Never exposed to the public internet.

use std::path::Path;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use noland_state_core::metrics::MetricsSnapshot;
use noland_state_core::*;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use uuid::Uuid;

pub const MAX_RPC_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcRequest {
    pub id: String,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcResponse {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthStatus {
    pub status: String,
    pub image_id: String,
    pub instance_id: String,
    pub socket: String,
    pub metrics: MetricsSnapshot,
    pub unfinished_operations: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActiveAppSession {
    pub session_id: Uuid,
    pub app_id: AppId,
    pub display_name: String,
    pub pids: Vec<i32>,
    pub started_at: DateTime<Utc>,
    pub identity_confidence: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetActiveAppSessionsResult {
    pub sessions: Vec<ActiveAppSession>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedProcessApp {
    pub app_id: AppId,
    pub display_name: String,
    pub session_id: Uuid,
    pub started_at: DateTime<Utc>,
    pub identity_confidence: f32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyBackupCommitResult {
    pub verified: bool,
    pub app_id: AppId,
    pub bundle_id: Uuid,
    pub commit_id: Uuid,
}

#[async_trait]
pub trait RpcHandler: Send + Sync {
    async fn handle(&self, request: &RpcRequest) -> Result<serde_json::Value>;
}

/// Calls one newline-framed RPC method over a Unix socket.
///
/// The caller owns timeout policy and can wrap this future in `tokio::time::timeout`.
pub async fn call(
    path: &Path,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value> {
    let request_id = Uuid::new_v4().to_string();
    let request = RpcRequest {
        id: request_id.clone(),
        method: method.to_string(),
        params,
    };
    let mut encoded = serde_json::to_vec(&request)?;
    encoded.push(b'\n');

    let mut stream = UnixStream::connect(path).await?;
    stream.write_all(&encoded).await?;
    stream.shutdown().await?;

    let mut response_bytes = Vec::new();
    let mut reader = BufReader::new(stream).take((MAX_RPC_RESPONSE_BYTES + 1) as u64);
    reader.read_until(b'\n', &mut response_bytes).await?;
    if response_bytes.len() > MAX_RPC_RESPONSE_BYTES {
        return Err(StateError::Invalid(format!(
            "RPC response exceeds {MAX_RPC_RESPONSE_BYTES} bytes"
        )));
    }
    if response_bytes.is_empty() {
        return Err(StateError::Invalid(
            "RPC server returned no response".into(),
        ));
    }
    if response_bytes.last() != Some(&b'\n') {
        return Err(StateError::Invalid(
            "RPC server returned an incomplete response".into(),
        ));
    }

    let response: RpcResponse = serde_json::from_slice(&response_bytes)?;
    if response.id != request_id {
        return Err(StateError::Invalid(format!(
            "RPC response id mismatch: expected {request_id}, got {}",
            response.id
        )));
    }
    if let Some(error) = response.error {
        return Err(StateError::Message(error));
    }
    response
        .result
        .ok_or_else(|| StateError::Invalid("RPC response contained no result".into()))
}

pub async fn bind_socket(path: &Path) -> Result<UnixListener> {
    if path.exists() {
        let _ = std::fs::remove_file(path);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let listener = UnixListener::bind(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660));
    }
    Ok(listener)
}

pub async fn serve_connection<H>(mut stream: UnixStream, handler: H) -> Result<()>
where
    H: RpcHandler,
{
    let (reader, mut writer) = stream.split();
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<RpcRequest>(&line) {
            Ok(req) => match handler.handle(&req).await {
                Ok(value) => RpcResponse {
                    id: req.id,
                    result: Some(value),
                    error: None,
                },
                Err(err) => RpcResponse {
                    id: req.id,
                    result: None,
                    error: Some(err.to_string()),
                },
            },
            Err(err) => RpcResponse {
                id: "unknown".into(),
                result: None,
                error: Some(err.to_string()),
            },
        };
        let mut encoded = serde_json::to_string(&response)?;
        encoded.push('\n');
        writer.write_all(encoded.as_bytes()).await?;
    }
    Ok(())
}

pub fn parse_uuid(raw: &str) -> Result<Uuid> {
    Uuid::parse_str(raw).map_err(|e| StateError::Invalid(e.to_string()))
}

pub fn method_name(raw: &str) -> &str {
    raw.trim()
}
