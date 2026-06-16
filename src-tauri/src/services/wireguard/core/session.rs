use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TunnelDriverKind {
    LinuxNative,
    MacosNative,
    WindowsNative,
    #[default]
    ManualApp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TunnelRuntimeState {
    #[default]
    Idle,
    Preparing,
    Provisioning,
    Starting,
    WaitingForHandshake,
    CheckingRoute,
    CheckingSunshine,
    Ready,
    Degraded,
    Recovering,
    Stopping,
    Stopped,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TunnelHealthStatus {
    #[default]
    Unknown,
    Healthy,
    Degraded,
    Unhealthy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelSession {
    pub tunnel_id: String,
    pub instance_id: Option<u64>,
    pub interface_name: String,
    pub client_tunnel_ip: String,
    pub server_tunnel_ip: String,
    pub client_public_key: String,
    pub server_public_key: String,
    pub endpoint_host: String,
    pub endpoint_port: u16,
    pub allowed_ips: String,
    pub mtu: u16,
    pub persistent_keepalive_secs: u16,
    pub sunshine_host: String,
    pub sunshine_port: u16,
    pub config_path: PathBuf,
    #[serde(skip_serializing, skip_deserializing, default)]
    pub config_text: String,
    #[serde(skip_serializing, skip_deserializing, default)]
    pub client_private_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelHealth {
    pub status: TunnelHealthStatus,
    pub sunshine_reachable: bool,
    pub last_handshake_at: Option<DateTime<Utc>>,
}

impl Default for TunnelHealth {
    fn default() -> Self {
        Self {
            status: TunnelHealthStatus::Unknown,
            sunshine_reachable: false,
            last_handshake_at: None,
        }
    }
}
