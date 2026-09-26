use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::{state::NetworkEndpoint, CONTROL_PROTOCOL_VERSION};

pub const CONTROL_SUBPROTOCOL: &str = "noland-network-control/1";

type HmacSha256 = Hmac<Sha256>;

pub fn auth_hello_mac(
    secret: &[u8; 32],
    protocol_version: u16,
    instance_id: &str,
    client_nonce: &str,
    timestamp: &str,
) -> String {
    control_mac(
        secret,
        &[
            b"auth-hello-v1",
            &protocol_version.to_be_bytes(),
            instance_id.as_bytes(),
            client_nonce.as_bytes(),
            timestamp.as_bytes(),
        ],
    )
}

pub fn auth_ok_mac(
    secret: &[u8; 32],
    client_nonce: &str,
    protocol_version: u16,
    control_session_id: Uuid,
    server_nonce: &str,
    next_sequence: u64,
) -> String {
    control_mac(
        secret,
        &[
            b"auth-ok-v1",
            client_nonce.as_bytes(),
            &protocol_version.to_be_bytes(),
            control_session_id.as_bytes(),
            server_nonce.as_bytes(),
            &next_sequence.to_be_bytes(),
        ],
    )
}

pub fn verify_auth_hello_mac(secret: &[u8; 32], hello: &AuthHello) -> bool {
    verify_control_mac(
        secret,
        &[
            b"auth-hello-v1",
            &hello.protocol_version.to_be_bytes(),
            hello.instance_id.as_bytes(),
            hello.client_nonce.as_bytes(),
            hello.timestamp.as_bytes(),
        ],
        &hello.mac,
    )
}

pub fn verify_auth_ok_mac(secret: &[u8; 32], client_nonce: &str, value: &AuthOk) -> bool {
    verify_control_mac(
        secret,
        &[
            b"auth-ok-v1",
            client_nonce.as_bytes(),
            &value.protocol_version.to_be_bytes(),
            value.control_session_id.as_bytes(),
            value.server_nonce.as_bytes(),
            &value.next_sequence.to_be_bytes(),
        ],
        &value.mac,
    )
}

fn control_mac(secret: &[u8; 32], fields: &[&[u8]]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts a 32-byte key");
    update_mac_fields(&mut mac, fields);
    hex::encode(mac.finalize().into_bytes())
}

fn verify_control_mac(secret: &[u8; 32], fields: &[&[u8]], encoded: &str) -> bool {
    let Ok(expected) = hex::decode(encoded) else {
        return false;
    };
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts a 32-byte key");
    update_mac_fields(&mut mac, fields);
    mac.verify_slice(&expected).is_ok()
}

fn update_mac_fields(mac: &mut HmacSha256, fields: &[&[u8]]) {
    for field in fields {
        let Ok(length) = u32::try_from(field.len()) else {
            return;
        };
        mac.update(&length.to_be_bytes());
        mac.update(field);
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthHello {
    pub protocol_version: u16,
    pub instance_id: String,
    pub client_nonce: String,
    pub timestamp: String,
    pub mac: String,
}

impl AuthHello {
    pub fn protocol_is_supported(&self) -> bool {
        self.protocol_version == CONTROL_PROTOCOL_VERSION
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthOk {
    pub protocol_version: u16,
    pub control_session_id: Uuid,
    pub server_nonce: String,
    pub next_sequence: u64,
    pub mac: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RpcRequest {
    pub protocol_version: u16,
    pub control_session_id: Uuid,
    pub request_id: Uuid,
    pub sequence: u64,
    pub method: String,
    pub expected_host_revision: Option<u64>,
    #[schemars(with = "serde_json::Value")]
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RpcResponse {
    pub protocol_version: u16,
    pub control_session_id: Uuid,
    pub request_id: Uuid,
    pub sequence: u64,
    pub host_revision: u64,
    #[schemars(with = "Option<serde_json::Value>")]
    pub result: Option<Value>,
    pub error: Option<crate::errors::NetworkError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PrepareTurnRequest {
    pub operation_id: Uuid,
    pub requested_generation: u64,
    pub turn_urls: Vec<String>,
    pub username: String,
    pub credential: String,
    pub credential_expires_at: String,
    pub expected_peer_ips: Vec<String>,
    pub effective_mtu: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PrepareTurnResponse {
    pub allocation_generation: u64,
    pub relay_endpoint: NetworkEndpoint,
    pub allocation_expires_at: Option<String>,
    pub credential_expires_at: String,
    pub permission_ips: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InstallProbeSessionRequest {
    pub probe_session_id: Uuid,
    pub token: String,
    pub expires_at: String,
    pub max_packets_per_second: u16,
    pub allowed_paths: Vec<crate::state::TransportKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HelperMethod {
    GetRuntime,
    SetPeerEndpoint,
    SetInterfaceMtu,
    ForceHandshake,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HelperRequest {
    pub request_id: Uuid,
    pub expected_launch_id: Uuid,
    pub expected_config_fingerprint: String,
    pub method: HelperMethod,
    #[schemars(with = "serde_json::Value")]
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HelperResponse {
    pub request_id: Uuid,
    pub launch_id: Option<Uuid>,
    #[schemars(with = "Option<serde_json::Value>")]
    pub result: Option<Value>,
    pub error: Option<HelperError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HelperError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SetPeerEndpointParams {
    pub peer_public_key: String,
    pub endpoint: NetworkEndpoint,
    pub transition_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SetInterfaceMtuParams {
    pub mtu: u16,
    pub transition_id: Uuid,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_auth_macs_round_trip_and_bind_every_field() {
        let secret = [0x42; 32];
        let mut hello = AuthHello {
            protocol_version: CONTROL_PROTOCOL_VERSION,
            instance_id: "123".into(),
            client_nonce: "ab".repeat(32),
            timestamp: "2026-09-26T12:00:00Z".into(),
            mac: String::new(),
        };
        hello.mac = auth_hello_mac(
            &secret,
            hello.protocol_version,
            &hello.instance_id,
            &hello.client_nonce,
            &hello.timestamp,
        );
        assert!(verify_auth_hello_mac(&secret, &hello));
        hello.instance_id = "124".into();
        assert!(!verify_auth_hello_mac(&secret, &hello));

        let mut ok = AuthOk {
            protocol_version: CONTROL_PROTOCOL_VERSION,
            control_session_id: Uuid::from_u128(7),
            server_nonce: "cd".repeat(32),
            next_sequence: 1,
            mac: String::new(),
        };
        ok.mac = auth_ok_mac(
            &secret,
            &"ab".repeat(32),
            ok.protocol_version,
            ok.control_session_id,
            &ok.server_nonce,
            ok.next_sequence,
        );
        assert!(verify_auth_ok_mac(&secret, &"ab".repeat(32), &ok));
        ok.next_sequence = 2;
        assert!(!verify_auth_ok_mac(&secret, &"ab".repeat(32), &ok));
    }
}
