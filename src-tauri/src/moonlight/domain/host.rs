use serde::{Deserialize, Serialize};

use super::config::StreamPreferencesPatch;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HostAddresses {
    pub overlay: Option<String>,
    pub lan: Option<String>,
    pub external: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AddressType {
    Overlay,
    Lan,
    External,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HostPorts {
    pub http: u16,
    pub https: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PairingStatus {
    Unpaired,
    Paired,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PersistedPairing {
    pub status: PairingStatus,
    pub server_certificate_pem: String,
    pub server_certificate_sha256: String,
    pub paired_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfoCache {
    pub app_version: String,
    pub gfe_version: Option<String>,
    pub server_codec_mode_support: u32,
    pub current_game_id: u32,
    pub last_seen_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CachedRemoteApp {
    pub id: u32,
    pub name: String,
    pub hdr_supported: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AppsCache {
    pub fetched_at: String,
    pub items: Vec<CachedRemoteApp>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PersistedHost {
    pub host_id: String,
    pub display_name: String,
    pub addresses: HostAddresses,
    pub active_address_type: AddressType,
    pub ports: HostPorts,
    pub pairing: Option<PersistedPairing>,
    pub server_info_cache: Option<ServerInfoCache>,
    pub apps_cache: Option<AppsCache>,
    pub preferences_override: Option<StreamPreferencesPatch>,
    pub last_selected_app_id: Option<u32>,
}
