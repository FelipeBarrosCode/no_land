use std::{net::SocketAddr, time::Duration};

use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use noland_network_contracts::{
    control::{
        auth_hello_mac, verify_auth_ok_mac, AuthHello, AuthOk, InstallProbeSessionRequest,
        PrepareTurnRequest, PrepareTurnResponse, RpcRequest, RpcResponse, CONTROL_SUBPROTOCOL,
    },
    state::HostNetworkState,
    CONTROL_PROTOCOL_VERSION,
};
use rand::{rngs::OsRng, RngCore};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use tokio::{net::TcpStream, time::timeout};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, http::HeaderValue, Message},
    MaybeTlsStream, WebSocketStream,
};
use uuid::Uuid;

use crate::errors::{AppError, AppResult};

use super::{network_agent::load_instance_control_secret, remote_exec::RemoteExec};

const CONTROL_TIMEOUT: Duration = Duration::from_secs(10);
const LOCAL_CONTROL_SOCKET: &str = "/run/noland-network-agent/control.sock";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostNetworkStatus {
    pub state: HostNetworkState,
    #[serde(default)]
    pub bridge: Option<TurnBridgeStatus>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnBridgeStatus {
    pub allocation_generation: u64,
    pub relay_endpoint: Option<SocketAddr>,
    #[serde(default)]
    pub allowed_peer_ips: Vec<String>,
    pub wireguard_packets_received: u64,
    pub wireguard_packets_sent: u64,
    pub probe_packets_received: u64,
    pub dropped_packets: u64,
    pub last_packet_at_unix_ms: Option<u64>,
    pub active_peer_tuple: Option<SocketAddr>,
}

pub struct NetworkControlClient {
    websocket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    control_session_id: Uuid,
    next_sequence: u64,
    host_revision: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LocalControlRequest {
    request_id: Uuid,
    method: String,
    params: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LocalControlResponse {
    request_id: Uuid,
    result: Option<Value>,
    error: Option<LocalControlError>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LocalControlError {
    code: String,
    message: String,
    retryable: bool,
}

impl NetworkControlClient {
    pub async fn connect(instance_id: u64, host: &str) -> AppResult<Self> {
        let secret = tokio::task::spawn_blocking(move || load_instance_control_secret(instance_id))
            .await
            .map_err(|error| AppError::State(format!("Secure storage task failed: {error}")))??;
        Self::connect_with_secret(&instance_id.to_string(), host, secret).await
    }

    async fn connect_with_secret(
        instance_id: &str,
        host: &str,
        secret: [u8; 32],
    ) -> AppResult<Self> {
        let host = host.trim();
        if host.is_empty() {
            return Err(AppError::InvalidInput(
                "Host control address is missing".to_string(),
            ));
        }
        let authority = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host.to_string()
        };
        let mut request = format!("ws://{authority}:6202")
            .into_client_request()
            .map_err(|error| AppError::Command(format!("Invalid host control URL: {error}")))?;
        request.headers_mut().insert(
            "sec-websocket-protocol",
            HeaderValue::from_static(CONTROL_SUBPROTOCOL),
        );
        let (mut websocket, response) = timeout(CONTROL_TIMEOUT, connect_async(request))
            .await
            .map_err(|_| AppError::Command("Host control connection timed out".to_string()))?
            .map_err(|error| {
                AppError::Command(format!("Could not connect to host control: {error}"))
            })?;
        let selected_protocol = response
            .headers()
            .get("sec-websocket-protocol")
            .and_then(|value| value.to_str().ok());
        if selected_protocol != Some(CONTROL_SUBPROTOCOL) {
            return Err(AppError::State(
                "Host did not negotiate the authenticated control protocol".to_string(),
            ));
        }

        let client_nonce = random_nonce();
        let timestamp = Utc::now().to_rfc3339();
        let hello = AuthHello {
            protocol_version: CONTROL_PROTOCOL_VERSION,
            instance_id: instance_id.to_string(),
            client_nonce: client_nonce.clone(),
            timestamp: timestamp.clone(),
            mac: auth_hello_mac(
                &secret,
                CONTROL_PROTOCOL_VERSION,
                instance_id,
                &client_nonce,
                &timestamp,
            ),
        };
        send_json(&mut websocket, &hello).await?;
        let auth_ok: AuthOk = receive_json(&mut websocket).await?;
        if auth_ok.protocol_version != CONTROL_PROTOCOL_VERSION
            || auth_ok.next_sequence == 0
            || !verify_auth_ok_mac(&secret, &client_nonce, &auth_ok)
        {
            return Err(AppError::State(
                "Host control authentication response was invalid".to_string(),
            ));
        }

        Ok(Self {
            websocket,
            control_session_id: auth_ok.control_session_id,
            next_sequence: auth_ok.next_sequence,
            host_revision: 0,
        })
    }

    pub async fn get_status(&mut self) -> AppResult<HostNetworkStatus> {
        self.call::<_, HostNetworkStatus>("get_network_status", &serde_json::json!({}), None)
            .await
    }

    pub async fn prepare_turn(
        &mut self,
        request: &PrepareTurnRequest,
    ) -> AppResult<PrepareTurnResponse> {
        self.call("prepare_turn", request, Some(self.host_revision))
            .await
    }

    pub async fn install_probe_session(
        &mut self,
        request: &InstallProbeSessionRequest,
    ) -> AppResult<()> {
        let _: Value = self
            .call("install_probe_session", request, Some(self.host_revision))
            .await?;
        Ok(())
    }

    pub async fn stop_turn(&mut self) -> AppResult<HostNetworkState> {
        self.call(
            "stop_turn",
            &serde_json::json!({}),
            Some(self.host_revision),
        )
        .await
    }

    async fn call<P: Serialize, R: DeserializeOwned>(
        &mut self,
        method: &str,
        params: &P,
        expected_host_revision: Option<u64>,
    ) -> AppResult<R> {
        let request = RpcRequest {
            protocol_version: CONTROL_PROTOCOL_VERSION,
            control_session_id: self.control_session_id,
            request_id: Uuid::new_v4(),
            sequence: self.next_sequence,
            method: method.to_string(),
            expected_host_revision,
            params: serde_json::to_value(params)?,
        };
        send_json(&mut self.websocket, &request).await?;
        let response: RpcResponse = receive_json(&mut self.websocket).await?;
        if response.protocol_version != CONTROL_PROTOCOL_VERSION
            || response.control_session_id != self.control_session_id
            || response.request_id != request.request_id
            || response.sequence != request.sequence
        {
            return Err(AppError::State(
                "Host control response identity did not match the request".to_string(),
            ));
        }
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.host_revision = response.host_revision;
        if let Some(error) = response.error {
            return Err(AppError::Command(format!(
                "Host network control failed ({:?}): {}",
                error.code, error.message
            )));
        }
        let result = response.result.ok_or_else(|| {
            AppError::State("Host control response omitted its result".to_string())
        })?;
        serde_json::from_value(result).map_err(AppError::from)
    }
}

pub async fn call_local_control_via_ssh<P: Serialize, R: DeserializeOwned>(
    remote: &RemoteExec,
    method: &str,
    params: &P,
) -> AppResult<R> {
    if !remote.is_root() && !remote.ssh_password.trim().is_empty() {
        return Err(AppError::Command(
            "Tunnel-independent host control requires root SSH or passwordless sudo".to_string(),
        ));
    }
    let request_id = Uuid::new_v4();
    let request = LocalControlRequest {
        request_id,
        method: method.to_string(),
        params: serde_json::to_value(params)?,
    };
    let mut input = serde_json::to_vec(&request)?;
    input.push(b'\n');
    let privilege = if remote.is_root() { "" } else { "sudo -n " };
    let command = format!(
        "{privilege}python3 -c 'import socket,sys; s=socket.socket(socket.AF_UNIX); s.settimeout(10); s.connect(\"{LOCAL_CONTROL_SOCKET}\"); s.sendall(sys.stdin.buffer.readline(65537)); sys.stdout.buffer.write(s.makefile(\"rb\").readline(65537))'"
    );
    let remote = remote.clone();
    let output = tokio::task::spawn_blocking(move || {
        remote.ssh_with_stdin(&command, input, Duration::from_secs(20))
    })
    .await
    .map_err(|error| AppError::Command(format!("SSH host control task failed: {error}")))??;
    if output.status_code != 0 {
        return Err(AppError::Command(format!(
            "Tunnel-independent host control failed: {}",
            output.stderr.trim()
        )));
    }
    if output.stdout.len() > 64 * 1024 {
        return Err(AppError::State(
            "Tunnel-independent host control response exceeded 64 KiB".to_string(),
        ));
    }
    let response: LocalControlResponse = serde_json::from_str(&output.stdout)?;
    if response.request_id != request_id {
        return Err(AppError::State(
            "Tunnel-independent host control response ID mismatch".to_string(),
        ));
    }
    if let Some(error) = response.error {
        return Err(AppError::Command(format!(
            "Host control failed ({}; retryable={}): {}",
            error.code, error.retryable, error.message
        )));
    }
    serde_json::from_value(response.result.ok_or_else(|| {
        AppError::State("Tunnel-independent host control omitted its result".to_string())
    })?)
    .map_err(AppError::from)
}

async fn send_json<T: Serialize>(
    websocket: &mut WebSocketStream<MaybeTlsStream<TcpStream>>,
    value: &T,
) -> AppResult<()> {
    let body = serde_json::to_string(value)?;
    timeout(CONTROL_TIMEOUT, websocket.send(Message::Text(body.into())))
        .await
        .map_err(|_| AppError::Command("Host control write timed out".to_string()))?
        .map_err(|error| AppError::Command(format!("Host control write failed: {error}")))
}

async fn receive_json<T: DeserializeOwned>(
    websocket: &mut WebSocketStream<MaybeTlsStream<TcpStream>>,
) -> AppResult<T> {
    let message = timeout(CONTROL_TIMEOUT, websocket.next())
        .await
        .map_err(|_| AppError::Command("Host control response timed out".to_string()))?
        .ok_or_else(|| AppError::Command("Host closed the control connection".to_string()))?
        .map_err(|error| AppError::Command(format!("Host control read failed: {error}")))?;
    let Message::Text(body) = message else {
        return Err(AppError::Serialization(
            "Host control response was not JSON text".to_string(),
        ));
    };
    serde_json::from_str(&body).map_err(AppError::from)
}

fn random_nonce() -> String {
    let mut nonce = [0_u8; 32];
    OsRng.fill_bytes(&mut nonce);
    hex::encode(nonce)
}
