use std::{path::PathBuf, sync::Arc};

use anyhow::{Context, Result};
use noland_network_contracts::control::{
    InstallProbeSessionRequest, PrepareTurnRequest, PrepareTurnResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
};
use uuid::Uuid;

use crate::turn_manager::TurnManager;

const MAX_REQUEST_BYTES: usize = 64 * 1024;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LocalControlRequest {
    request_id: Uuid,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LocalControlResponse {
    request_id: Uuid,
    result: Option<Value>,
    error: Option<LocalControlError>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LocalControlError {
    code: String,
    message: String,
    retryable: bool,
}

pub async fn run(socket_path: PathBuf, manager: Arc<TurnManager>) -> std::io::Result<()> {
    if let Some(parent) = socket_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let _ = tokio::fs::remove_file(&socket_path).await;
    let listener = UnixListener::bind(&socket_path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600)).await?;
    }

    loop {
        let (stream, _) = listener.accept().await?;
        let manager = manager.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_connection(stream, manager).await {
                eprintln!("No Land network agent control request failed: {error:#}");
            }
        });
    }
}

async fn handle_connection(stream: UnixStream, manager: Arc<TurnManager>) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut body = Vec::new();
    (&mut reader)
        .take((MAX_REQUEST_BYTES + 1) as u64)
        .read_until(b'\n', &mut body)
        .await
        .context("failed reading local control request")?;
    let response = match decode_local_request(&body) {
        Ok(request) => dispatch(request, manager).await,
        Err(response) => response,
    };
    writer.write_all(&serde_json::to_vec(&response)?).await?;
    writer.write_all(b"\n").await?;
    writer.shutdown().await?;
    Ok(())
}

fn decode_local_request(
    body: &[u8],
) -> std::result::Result<LocalControlRequest, LocalControlResponse> {
    if body.len() > MAX_REQUEST_BYTES {
        return Err(error_response(
            Uuid::nil(),
            "request_too_large",
            "control request exceeds 64 KiB",
            false,
        ));
    }
    if !body.ends_with(b"\n") {
        return Err(error_response(
            Uuid::nil(),
            "invalid_request",
            "control request must end with a newline",
            false,
        ));
    }
    serde_json::from_slice(body).map_err(|error| {
        error_response(
            Uuid::nil(),
            "invalid_request",
            &format!("invalid control request: {error}"),
            false,
        )
    })
}

async fn dispatch(request: LocalControlRequest, manager: Arc<TurnManager>) -> LocalControlResponse {
    let result = dispatch_method(&request.method, request.params, manager).await;

    match result {
        Ok(result) => LocalControlResponse {
            request_id: request.request_id,
            result: Some(result),
            error: None,
        },
        Err(error) => error_response(
            request.request_id,
            "control_operation_failed",
            &error.to_string(),
            true,
        ),
    }
}

pub async fn dispatch_method(
    method: &str,
    params: Value,
    manager: Arc<TurnManager>,
) -> Result<Value> {
    match method {
        "get_capabilities" => Ok(serde_json::json!({
            "agentVersion": env!("CARGO_PKG_VERSION"),
            "protocolVersions": [1],
            "turnProviders": ["cloudflare"],
            "turnTransports": ["udp"],
            "probeProtocolVersions": [1, 2],
            "bridgeSupported": true,
            "policyProfiles": ["gaming-v1"],
        })),
        "prepare_turn" => match serde_json::from_value::<PrepareTurnRequest>(params) {
            Ok(params) => {
                manager
                    .prepare_turn(params)
                    .await
                    .and_then(|result: PrepareTurnResponse| {
                        serde_json::to_value(result).map_err(Into::into)
                    })
            }
            Err(error) => Err(anyhow::anyhow!("invalid prepare_turn parameters: {error}")),
        },
        "get_network_status" => manager.status().await.and_then(|(state, bridge)| {
            serde_json::to_value(serde_json::json!({
                "state": state,
                "bridge": bridge,
            }))
            .map_err(Into::into)
        }),
        "install_probe_session" => {
            match serde_json::from_value::<InstallProbeSessionRequest>(params) {
                Ok(params) => manager
                    .install_probe_session(params)
                    .await
                    .map(|()| serde_json::json!({ "installed": true })),
                Err(error) => Err(anyhow::anyhow!(
                    "invalid install_probe_session parameters: {error}"
                )),
            }
        }
        "stop_turn" => manager
            .stop_turn()
            .await
            .and_then(|state| serde_json::to_value(state).map_err(Into::into)),
        _ => Err(anyhow::anyhow!("unsupported control method `{method}`")),
    }
}

fn error_response(
    request_id: Uuid,
    code: &str,
    message: &str,
    retryable: bool,
) -> LocalControlResponse {
    LocalControlResponse {
        request_id,
        result: None,
        error: Some(LocalControlError {
            code: code.to_string(),
            message: message.to_string(),
            retryable,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_oversized_malformed_and_unframed_requests() {
        let oversized = vec![b'x'; MAX_REQUEST_BYTES + 1];
        assert_eq!(
            decode_local_request(&oversized)
                .unwrap_err()
                .error
                .unwrap()
                .code,
            "request_too_large"
        );
        assert_eq!(
            decode_local_request(b"{}").unwrap_err().error.unwrap().code,
            "invalid_request"
        );
        assert_eq!(
            decode_local_request(b"not-json\n")
                .unwrap_err()
                .error
                .unwrap()
                .code,
            "invalid_request"
        );
    }

    #[test]
    fn accepts_one_newline_delimited_request() {
        let id = Uuid::from_u128(9);
        let body = format!(
            "{{\"requestId\":\"{id}\",\"method\":\"get_network_status\",\"params\":{{}}}}\n"
        );
        let request = decode_local_request(body.as_bytes()).unwrap();
        assert_eq!(request.request_id, id);
        assert_eq!(request.method, "get_network_status");
    }
}
