//! Local Unix-socket JSON-RPC. Never exposed to the public internet.

use std::path::Path;

use async_trait::async_trait;
use noland_state_core::metrics::MetricsSnapshot;
use noland_state_core::*;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use uuid::Uuid;

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

#[async_trait]
pub trait RpcHandler: Send + Sync {
    async fn handle(&self, request: &RpcRequest) -> Result<serde_json::Value>;
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
