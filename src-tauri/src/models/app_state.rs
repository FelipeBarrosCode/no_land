use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedAppState {
    pub version: u32,
    pub onboarding_completed: bool,
    pub credentials: CredentialsState,
    pub ssh: SshState,
    pub location: LocationState,
    pub server_preferences: ServerPreferences,
    pub selected_offer: Option<OfferCandidate>,
    pub instance: InstanceState,
    pub wireguard: WireGuardState,
    pub sunshine: SunshineState,
    pub moonlight: MoonlightState,
    pub moonlight_preferences: MoonlightPreferences,
    pub provisioned_servers: Vec<ProvisionedServerState>,
    pub orchestration_state: OrchestrationState,
    pub last_error: Option<String>,
}

impl Default for PersistedAppState {
    fn default() -> Self {
        Self {
            version: 1,
            onboarding_completed: false,
            credentials: CredentialsState::default(),
            ssh: SshState::default(),
            location: LocationState::default(),
            server_preferences: ServerPreferences::default(),
            selected_offer: None,
            instance: InstanceState::default(),
            wireguard: WireGuardState::default(),
            sunshine: SunshineState::default(),
            moonlight: MoonlightState::default(),
            moonlight_preferences: MoonlightPreferences::default(),
            provisioned_servers: Vec::new(),
            orchestration_state: OrchestrationState::Idle,
            last_error: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CredentialsState {
    pub app_username: String,
    pub app_password: String,
    pub vast_api_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshState {
    pub key_name: String,
    pub private_key_path: String,
    pub public_key_path: String,
    pub uploaded_to_vast: bool,
    pub ssh_username: String,
    pub ssh_password: String,
}

impl Default for SshState {
    fn default() -> Self {
        Self {
            key_name: "nolandConnectSSH".to_string(),
            private_key_path: String::new(),
            public_key_path: String::new(),
            uploaded_to_vast: false,
            ssh_username: "root".to_string(),
            ssh_password: String::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LocationSource {
    Os,
    Ip,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocationState {
    pub source: LocationSource,
    pub city: String,
    pub region: String,
    pub country: String,
    pub latitude: f64,
    pub longitude: f64,
}

impl Default for LocationState {
    fn default() -> Self {
        Self {
            source: LocationSource::Ip,
            city: String::new(),
            region: String::new(),
            country: String::new(),
            latitude: 0.0,
            longitude: 0.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerPreferences {
    pub min_reliability: f64,
    pub storage_gb: u32,
    pub template_hash: String,
    pub max_hourly_price: f64,
    pub min_hourly_price: f64,
    pub require_verified: bool,
    pub require_datacenter: bool,
    #[serde(default = "default_true")]
    pub include_on_demand: bool,
    #[serde(default = "default_true")]
    pub include_interruptible: bool,
    #[serde(default = "default_true")]
    pub include_reserved: bool,
    #[serde(default)]
    pub require_static_ip: bool,
    #[serde(default)]
    pub require_avx: bool,
    #[serde(default)]
    pub min_gpu_count: u32,
    #[serde(default)]
    pub min_gpu_ram_gb: u32,
    #[serde(default)]
    pub min_cpu_cores: f64,
    #[serde(default)]
    pub min_inet_down_mbps: f64,
    #[serde(default)]
    pub min_inet_up_mbps: f64,
    #[serde(default)]
    pub geolocation_country_code: String,
}

impl Default for ServerPreferences {
    fn default() -> Self {
        Self {
            min_reliability: 0.8,
            storage_gb: 100,
            template_hash: "2a62a7d5089a50a5ad89a9480f540d25".to_string(),
            max_hourly_price: 0.0, // 0 means no limit
            min_hourly_price: 0.0,
            require_verified: false,
            require_datacenter: false,
            include_on_demand: true,
            include_interruptible: true,
            include_reserved: true,
            require_static_ip: false,
            require_avx: false,
            min_gpu_count: 1,
            min_gpu_ram_gb: 0,
            min_cpu_cores: 0.0,
            min_inet_down_mbps: 0.0,
            min_inet_up_mbps: 0.0,
            geolocation_country_code: "US".to_string(),
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OfferCandidate {
    pub id: u64,
    pub host_id: Option<u64>,
    pub host_label: String,
    pub location_label: String,
    pub city: String,
    pub region: String,
    pub country: String,
    pub latitude: f64,
    pub longitude: f64,
    pub reliability: f64,
    pub gpu_name: String,
    pub gpu_ram_mb: u64,
    pub gpu_count: u32,
    #[serde(default)]
    pub cpu_name: String,
    #[serde(default)]
    pub cpu_cores: f64,
    #[serde(default)]
    pub internet_down_mbps: f64,
    #[serde(default)]
    pub internet_up_mbps: f64,
    pub hourly_price: f64,
    pub available_storage_gb: u32,
    pub estimated_distance_km: f64,
    pub score: f64,
    #[serde(default)]
    pub time_remaining_hours: f64,
    #[serde(default)]
    pub is_verified: bool,
    #[serde(default)]
    pub is_datacenter: bool,
    #[serde(default)]
    pub offer_type: String,
    #[serde(default)]
    pub has_static_ip: bool,
    #[serde(default)]
    pub has_avx: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceState {
    pub instance_id: Option<u64>,
    pub offer_id: Option<u64>,
    pub status: String,
    pub ssh_host: String,
    pub ssh_port: u16,
    pub ssh_user: String,
    #[serde(default)]
    pub ssh_command: String,
}

impl Default for InstanceState {
    fn default() -> Self {
        Self {
            instance_id: None,
            offer_id: None,
            status: "idle".to_string(),
            ssh_host: String::new(),
            ssh_port: 22,
            ssh_user: "root".to_string(),
            ssh_command: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WireGuardState {
    pub server_ip: String,
    pub client_ip: String,
    pub server_public_key: String,
    pub client_public_key: String,
    pub config_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SunshineState {
    pub configured: bool,
}

impl Default for SunshineState {
    fn default() -> Self {
        Self { configured: false }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoonlightState {
    pub configured: bool,
    pub host_address: String,
}

impl Default for MoonlightState {
    fn default() -> Self {
        Self {
            configured: false,
            host_address: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoonlightPreferences {
    pub bitrate: u32,
    pub fps: u32,
    pub width: u32,
    pub height: u32,
    pub hostaudio: u8,
    pub showperfoverlay: u8,
    pub keepawake: u8,
    pub framepacing: u8,
    pub vsync: u8,
    pub hdr: u8,
    pub videocfg: u8,
    pub videodec: u8,
    pub yuv444: u8,
    pub gameopts: u8,
    pub gamepadmouse: u8,
    pub detectnetblocking: u8,
}

impl Default for MoonlightPreferences {
    fn default() -> Self {
        Self {
            bitrate: 80000,
            fps: 60,
            width: 2560,
            height: 1440,
            hostaudio: 1,
            showperfoverlay: 1,
            keepawake: 1,
            framepacing: 1,
            vsync: 1,
            hdr: 0,
            videocfg: 2,
            videodec: 1,
            yuv444: 0,
            gameopts: 1,
            gamepadmouse: 1,
            detectnetblocking: 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerPreferencesUpdate {
    pub min_reliability: f64,
    pub storage_gb: u32,
    pub template_hash: String,
    pub max_hourly_price: f64,
    pub min_hourly_price: f64,
    pub require_verified: bool,
    pub require_datacenter: bool,
    pub include_on_demand: bool,
    pub include_interruptible: bool,
    pub include_reserved: bool,
    pub require_static_ip: bool,
    pub require_avx: bool,
    pub min_gpu_count: u32,
    pub min_gpu_ram_gb: u32,
    pub min_cpu_cores: f64,
    pub min_inet_down_mbps: f64,
    pub min_inet_up_mbps: f64,
    pub geolocation_country_code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RentedInstanceSummary {
    pub instance_id: u64,
    pub label: String,
    pub status: String,
    pub gpu_name: String,
    pub ssh_host: String,
    pub ssh_port: u16,
    pub public_ip: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionedServerState {
    pub instance_id: u64,
    pub offer_id: Option<u64>,
    pub ssh_host: String,
    pub ssh_port: u16,
    pub status: String,
    #[serde(default)]
    pub ssh_command: String,
    #[serde(default)]
    pub wireguard_server_ip: String,
    #[serde(default)]
    pub wireguard_client_ip: String,
    #[serde(default)]
    pub wireguard_server_public_key: String,
    #[serde(default)]
    pub wireguard_client_public_key: String,
    #[serde(default)]
    pub wireguard_config_path: String,
    #[serde(default)]
    pub moonlight_host_address: String,
    pub last_state: OrchestrationState,
    pub last_error: Option<String>,
    pub steps: ProvisionedServerSteps,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionedServerSteps {
    pub ssh_key_ready: bool,
    pub ssh_key_uploaded_to_vast: bool,
    pub instance_created: bool,
    pub instance_ready: bool,
    pub ssh_connected: bool,
    pub nvidia_headless_configured: bool,
    #[serde(default)]
    pub post_nvidia_reboot_completed: bool,
    pub sunshine_configured: bool,
    pub low_latency_audio_configured: bool,
    pub wireguard_configured: bool,
    pub moonlight_configured: bool,
    pub awaiting_pair_pin: bool,
    pub pairing_completed: bool,
}

impl ProvisionedServerState {
    pub fn new(instance_id: u64) -> Self {
        Self {
            instance_id,
            offer_id: None,
            ssh_host: String::new(),
            ssh_port: 22,
            status: "unknown".to_string(),
            ssh_command: String::new(),
            wireguard_server_ip: String::new(),
            wireguard_client_ip: String::new(),
            wireguard_server_public_key: String::new(),
            wireguard_client_public_key: String::new(),
            wireguard_config_path: String::new(),
            moonlight_host_address: String::new(),
            last_state: OrchestrationState::Idle,
            last_error: None,
            steps: ProvisionedServerSteps::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum OrchestrationState {
    Idle,
    Onboarding,
    SelectingServer,
    ServerSelected,
    GeneratingSshKey,
    UploadingSshKeyToVast,
    CreatingInstance,
    WaitingForInstance,
    VerifyingReservation,
    ConnectingSsh,
    ConfiguringRemote,
    ConfiguringWireGuard,
    ConfiguringSunshine,
    ConfiguringNvidiaHeadless,
    ConfiguringMoonlight,
    AwaitingPairPin,
    Pairing,
    Ready,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OnboardingPayload {
    pub app_username: String,
    pub app_password: String,
    pub vast_api_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualLocationInput {
    pub city: String,
    pub region: String,
    pub country: String,
    pub latitude: f64,
    pub longitude: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingContext {
    pub host: String,
    pub port: u16,
    pub user: String,
}
