use std::{
    collections::VecDeque,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use futures_util::{SinkExt, StreamExt};
use noland_network_contracts::{
    control::{auth_ok_mac, verify_auth_hello_mac, AuthHello, AuthOk, RpcRequest, RpcResponse},
    errors::{NetworkError, NetworkErrorCode},
    CONTROL_PROTOCOL_VERSION,
};
use rand::{rngs::OsRng, RngCore};
use tokio::{net::TcpStream, sync::Mutex};
use tokio_tungstenite::{tungstenite::Message, WebSocketStream};
use uuid::Uuid;

use crate::{control::dispatch_method, turn_manager::TurnManager};

const AUTH_CLOCK_SKEW: chrono::Duration = chrono::Duration::seconds(60);
const NONCE_RETENTION: Duration = Duration::from_secs(120);
const MAX_RETAINED_NONCES: usize = 1024;
const MAX_RPC_BYTES: usize = 64 * 1024;

pub struct RemoteControl {
    instance_id: String,
    secret: [u8; 32],
    manager: Arc<TurnManager>,
    accepted_nonces: Mutex<VecDeque<(String, Instant)>>,
}

impl RemoteControl {
    pub fn new(instance_id: String, secret: [u8; 32], manager: Arc<TurnManager>) -> Arc<Self> {
        Arc::new(Self {
            instance_id,
            secret,
            manager,
            accepted_nonces: Mutex::new(VecDeque::new()),
        })
    }

    pub async fn handle(
        &self,
        mut websocket: WebSocketStream<TcpStream>,
        hello: AuthHello,
    ) -> Result<()> {
        self.authenticate(&hello).await?;

        let control_session_id = Uuid::new_v4();
        let server_nonce = random_nonce();
        let next_sequence = 1;
        let auth_ok = AuthOk {
            protocol_version: CONTROL_PROTOCOL_VERSION,
            control_session_id,
            server_nonce: server_nonce.clone(),
            next_sequence,
            mac: auth_ok_mac(
                &self.secret,
                &hello.client_nonce,
                CONTROL_PROTOCOL_VERSION,
                control_session_id,
                &server_nonce,
                next_sequence,
            ),
        };
        websocket
            .send(Message::Text(serde_json::to_string(&auth_ok)?.into()))
            .await?;

        let mut expected_sequence = next_sequence;
        while let Some(message) = websocket.next().await {
            let message = message?;
            let text = match message {
                Message::Text(text) if text.len() <= MAX_RPC_BYTES => text,
                Message::Close(_) => return Ok(()),
                _ => {
                    self.send_error(
                        &mut websocket,
                        control_session_id,
                        Uuid::nil(),
                        expected_sequence,
                        NetworkErrorCode::InvalidRequest,
                        "control requests must be JSON text no larger than 64 KiB",
                        false,
                    )
                    .await?;
                    return Ok(());
                }
            };
            let request = match serde_json::from_str::<RpcRequest>(&text) {
                Ok(request) => request,
                Err(_) => {
                    self.send_error(
                        &mut websocket,
                        control_session_id,
                        Uuid::nil(),
                        expected_sequence,
                        NetworkErrorCode::InvalidRequest,
                        "invalid control request",
                        false,
                    )
                    .await?;
                    return Ok(());
                }
            };

            if request.protocol_version != CONTROL_PROTOCOL_VERSION {
                self.send_request_error(
                    &mut websocket,
                    &request,
                    NetworkErrorCode::UnsupportedProtocolVersion,
                    "unsupported control protocol version",
                    false,
                )
                .await?;
                return Ok(());
            }
            if request.control_session_id != control_session_id {
                self.send_request_error(
                    &mut websocket,
                    &request,
                    NetworkErrorCode::Unauthorized,
                    "control session does not match this connection",
                    false,
                )
                .await?;
                return Ok(());
            }
            if request.sequence != expected_sequence {
                self.send_request_error(
                    &mut websocket,
                    &request,
                    NetworkErrorCode::ReplayedRequest,
                    "control request sequence is stale or out of order",
                    false,
                )
                .await?;
                return Ok(());
            }

            let revision_before = self.manager.host_revision().await;
            if is_mutating_method(&request.method)
                && request
                    .expected_host_revision
                    .is_some_and(|expected| expected != revision_before)
            {
                self.send_request_error(
                    &mut websocket,
                    &request,
                    NetworkErrorCode::StaleRevision,
                    "host network state changed before this request",
                    true,
                )
                .await?;
                expected_sequence = expected_sequence.saturating_add(1);
                continue;
            }

            let result = dispatch_method(
                &request.method,
                request.params.clone(),
                self.manager.clone(),
            )
            .await;
            let host_revision = self.manager.host_revision().await;
            let response = match result {
                Ok(result) => RpcResponse {
                    protocol_version: CONTROL_PROTOCOL_VERSION,
                    control_session_id,
                    request_id: request.request_id,
                    sequence: request.sequence,
                    host_revision,
                    result: Some(result),
                    error: None,
                },
                Err(error) => RpcResponse {
                    protocol_version: CONTROL_PROTOCOL_VERSION,
                    control_session_id,
                    request_id: request.request_id,
                    sequence: request.sequence,
                    host_revision,
                    result: None,
                    error: Some(NetworkError {
                        code: if error.to_string().starts_with("unsupported control method") {
                            NetworkErrorCode::UnsupportedMethod
                        } else {
                            NetworkErrorCode::ControlOperationFailed
                        },
                        message: error.to_string(),
                        retryable: true,
                        transport: None,
                        transition_id: None,
                        details: None,
                    }),
                },
            };
            websocket
                .send(Message::Text(serde_json::to_string(&response)?.into()))
                .await?;
            expected_sequence = expected_sequence.saturating_add(1);
        }
        Ok(())
    }

    async fn authenticate(&self, hello: &AuthHello) -> Result<()> {
        if !hello.protocol_is_supported() {
            bail!("unsupported control protocol version");
        }
        if hello.instance_id != self.instance_id {
            bail!("control instance identity mismatch");
        }
        if hex::decode(&hello.client_nonce)
            .ok()
            .is_none_or(|nonce| nonce.len() != 32)
        {
            bail!("invalid control client nonce");
        }
        let timestamp = DateTime::parse_from_rfc3339(&hello.timestamp)
            .context("invalid control authentication timestamp")?
            .with_timezone(&Utc);
        let skew = timestamp.signed_duration_since(Utc::now()).abs();
        if skew > AUTH_CLOCK_SKEW {
            bail!("control authentication timestamp is outside the allowed clock skew");
        }
        if !verify_auth_hello_mac(&self.secret, hello) {
            bail!("invalid control authentication MAC");
        }

        let now = Instant::now();
        let mut nonces = self.accepted_nonces.lock().await;
        while nonces
            .front()
            .is_some_and(|(_, accepted)| now.duration_since(*accepted) >= NONCE_RETENTION)
        {
            nonces.pop_front();
        }
        if nonces.iter().any(|(nonce, _)| nonce == &hello.client_nonce) {
            bail!("replayed control authentication nonce");
        }
        while nonces.len() >= MAX_RETAINED_NONCES {
            nonces.pop_front();
        }
        nonces.push_back((hello.client_nonce.clone(), now));
        Ok(())
    }

    async fn send_request_error(
        &self,
        websocket: &mut WebSocketStream<TcpStream>,
        request: &RpcRequest,
        code: NetworkErrorCode,
        message: &str,
        retryable: bool,
    ) -> Result<()> {
        self.send_error(
            websocket,
            request.control_session_id,
            request.request_id,
            request.sequence,
            code,
            message,
            retryable,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn send_error(
        &self,
        websocket: &mut WebSocketStream<TcpStream>,
        control_session_id: Uuid,
        request_id: Uuid,
        sequence: u64,
        code: NetworkErrorCode,
        message: &str,
        retryable: bool,
    ) -> Result<()> {
        let response = RpcResponse {
            protocol_version: CONTROL_PROTOCOL_VERSION,
            control_session_id,
            request_id,
            sequence,
            host_revision: self.manager.host_revision().await,
            result: None,
            error: Some(NetworkError {
                code,
                message: message.to_string(),
                retryable,
                transport: None,
                transition_id: None,
                details: None,
            }),
        };
        websocket
            .send(Message::Text(serde_json::to_string(&response)?.into()))
            .await?;
        Ok(())
    }
}

pub fn load_control_secret(path: &Path) -> Result<Option<[u8; 32]>> {
    let body = match std::fs::read_to_string(path) {
        Ok(body) => body,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("failed reading control secret"),
    };
    let decoded = hex::decode(body.trim()).context("control secret must be hexadecimal")?;
    let secret = decoded
        .try_into()
        .map_err(|_: Vec<u8>| anyhow::anyhow!("control secret must encode exactly 32 bytes"))?;
    Ok(Some(secret))
}

fn random_nonce() -> String {
    let mut nonce = [0_u8; 32];
    OsRng.fill_bytes(&mut nonce);
    hex::encode(nonce)
}

fn is_mutating_method(method: &str) -> bool {
    matches!(
        method,
        "prepare_turn" | "install_probe_session" | "stop_turn"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use noland_network_contracts::control::auth_hello_mac;

    use crate::shared_state;

    #[test]
    fn only_state_changing_methods_require_revision_checks() {
        assert!(is_mutating_method("prepare_turn"));
        assert!(is_mutating_method("install_probe_session"));
        assert!(!is_mutating_method("get_network_status"));
        assert!(!is_mutating_method("get_capabilities"));
    }

    #[tokio::test]
    async fn authentication_binds_instance_time_mac_and_rejects_nonce_replay() {
        let root = std::env::temp_dir().join(format!("noland-control-test-{}", Uuid::new_v4()));
        let manager = TurnManager::new(
            "42".to_string(),
            "test".to_string(),
            root.join("state.json"),
            "127.0.0.1:51820".parse().unwrap(),
            shared_state(4, 20),
        )
        .unwrap();
        let secret = [0x44; 32];
        let control = RemoteControl::new("42".to_string(), secret, manager);
        let client_nonce = "ab".repeat(32);
        let timestamp = Utc::now().to_rfc3339();
        let hello = AuthHello {
            protocol_version: CONTROL_PROTOCOL_VERSION,
            instance_id: "42".to_string(),
            client_nonce: client_nonce.clone(),
            timestamp: timestamp.clone(),
            mac: auth_hello_mac(
                &secret,
                CONTROL_PROTOCOL_VERSION,
                "42",
                &client_nonce,
                &timestamp,
            ),
        };
        control.authenticate(&hello).await.unwrap();
        assert!(control.authenticate(&hello).await.is_err());

        let mut wrong_instance = hello;
        wrong_instance.client_nonce = "cd".repeat(32);
        wrong_instance.instance_id = "43".to_string();
        wrong_instance.mac = auth_hello_mac(
            &secret,
            CONTROL_PROTOCOL_VERSION,
            "43",
            &wrong_instance.client_nonce,
            &wrong_instance.timestamp,
        );
        assert!(control.authenticate(&wrong_instance).await.is_err());
        let _ = std::fs::remove_dir_all(root);
    }
}
