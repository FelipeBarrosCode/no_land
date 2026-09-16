use std::path::{Path, PathBuf};
use std::sync::Arc;

use noland_rpc::{RpcRequest, RpcResponse};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tracing::{info, warn};

use crate::activity::{ActivityEvent, ActivityRecorder};
use crate::engine::LifecycleEngine;
use crate::{AgentError, Result};

const MAX_RPC_LINE_BYTES: usize = 64 * 1024;

pub struct LifecycleRpcService {
    engine: Arc<LifecycleEngine>,
    activity: Arc<dyn ActivityRecorder>,
    config_path: PathBuf,
}

impl LifecycleRpcService {
    pub fn new(
        engine: Arc<LifecycleEngine>,
        activity: Arc<dyn ActivityRecorder>,
        config_path: PathBuf,
    ) -> Self {
        Self {
            engine,
            activity,
            config_path,
        }
    }

    async fn handle(&self, request: RpcRequest) -> RpcResponse {
        let result = match request.method.as_str() {
            "GetHealth" => self.engine.status().map(|status| {
                json!({
                    "status": if status.last_error.is_some() { "degraded" } else { "ok" },
                    "instanceId": status.instance_id,
                    "enabled": status.enabled,
                    "state": status.state,
                })
            }),
            "GetStatus" => self
                .engine
                .status()
                .and_then(|status| serde_json::to_value(status).map_err(Into::into)),
            "RecordActivity" => match serde_json::from_value::<ActivityEvent>(request.params) {
                Ok(event) => self
                    .activity
                    .record_activity(event)
                    .await
                    .and_then(|outcome| {
                        serde_json::to_value(format!("{outcome:?}"))
                            .map(|value| json!({"outcome": value}))
                            .map_err(Into::into)
                    }),
                Err(error) => Err(AgentError::new(format!("invalid activity event: {error}"))),
            },
            "ReloadConfig" => self
                .engine
                .reload_config(&self.config_path)
                .map(|_| json!({"reloaded": true})),
            _ => Err(AgentError::new("unknown lifecycle RPC method")),
        };
        match result {
            Ok(value) => RpcResponse {
                id: request.id,
                result: Some(value),
                error: None,
            },
            Err(error) => RpcResponse {
                id: request.id,
                result: None,
                error: Some(error.to_string()),
            },
        }
    }
}

pub async fn serve_status_socket(path: &Path, service: Arc<LifecycleRpcService>) -> Result<()> {
    let listener = bind_socket(path).await?;
    info!(path = %path.display(), "lifecycle status socket listening");
    loop {
        let (stream, _) = listener.accept().await?;
        let service = Arc::clone(&service);
        tokio::spawn(async move {
            if let Err(error) = serve_connection(stream, service).await {
                warn!(error = %error, "status RPC client disconnected with an error");
            }
        });
    }
}

async fn bind_socket(path: &Path) -> Result<UnixListener> {
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let listener = UnixListener::bind(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))?;
    }
    Ok(listener)
}

async fn serve_connection(stream: UnixStream, service: Arc<LifecycleRpcService>) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = Vec::with_capacity(1024);
    loop {
        line.clear();
        let bytes = reader.read_until(b'\n', &mut line).await?;
        if bytes == 0 {
            return Ok(());
        }
        if line.len() > MAX_RPC_LINE_BYTES || !line.ends_with(b"\n") {
            let response = RpcResponse {
                id: "unknown".into(),
                result: None,
                error: Some("RPC request exceeds line limit".into()),
            };
            write_response(&mut writer, &response).await?;
            continue;
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let response = match serde_json::from_slice::<RpcRequest>(&line) {
            Ok(request) => service.handle(request).await,
            Err(error) => RpcResponse {
                id: "unknown".into(),
                result: None,
                error: Some(format!("invalid RPC request: {error}")),
            },
        };
        write_response(&mut writer, &response).await?;
    }
}

async fn write_response(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    response: &RpcResponse,
) -> Result<()> {
    let mut encoded = serde_json::to_vec(response)?;
    encoded.push(b'\n');
    writer.write_all(&encoded).await?;
    Ok(())
}
