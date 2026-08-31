use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedAppState {
    pub version: u32,
    pub onboarding_completed: bool,
    #[serde(default)]
    pub has_completed_guided_setup: bool,
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
    pub shared_storage: SharedStorageState,
    #[serde(default)]
    pub shared_storage_profiles: Vec<crate::models::application_bundle::ProfileReference>,
    #[serde(default)]
    pub shared_storage_credentials: SharedStorageCredentialState,
    pub provisioned_servers: Vec<ProvisionedServerState>,
    #[serde(default)]
    pub post_wireguard_setup: PostWireGuardSetupState,
    pub orchestration_state: OrchestrationState,
    #[serde(default)]
    pub connection_provider: ConnectionProvider,
    pub last_error: Option<String>,
}

impl Default for PersistedAppState {
    fn default() -> Self {
        Self {
            version: 2,
            onboarding_completed: false,
            has_completed_guided_setup: false,
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
            shared_storage: SharedStorageState::default(),
            shared_storage_profiles: Vec::new(),
            shared_storage_credentials: SharedStorageCredentialState::default(),
            provisioned_servers: Vec::new(),
            post_wireguard_setup: PostWireGuardSetupState::default(),
            orchestration_state: OrchestrationState::Idle,
            connection_provider: ConnectionProvider::default(),
            last_error: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialsState {
    pub app_username: String,
    pub app_password: String,
    pub vast_api_key: String,
    #[serde(default)]
    pub twitch_client_id: String,
    #[serde(default)]
    pub twitch_client_secret: String,
}

impl Default for CredentialsState {
    fn default() -> Self {
        Self {
            app_username: String::new(),
            app_password: String::new(),
            vast_api_key: String::new(),
            twitch_client_id: String::new(),
            twitch_client_secret: String::new(),
        }
    }
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
            ssh_password: "user".to_string(),
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
            template_hash: "566868bff8b15eef891ee706acbbb5e5".to_string(),
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

fn default_mic_device_id() -> String {
    "default".to_string()
}

fn default_mic_device_name() -> String {
    "System Default".to_string()
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
    #[serde(default)]
    pub client_private_key_fingerprint: String,
    #[serde(default)]
    pub client_public_key_fingerprint: String,
    #[serde(default)]
    pub server_public_key_fingerprint: String,
    #[serde(default)]
    pub endpoint_host: String,
    #[serde(default)]
    pub endpoint_port: u16,
    #[serde(default)]
    pub last_runtime_interface: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionProvider {
    #[default]
    #[serde(alias = "gotatun", alias = "tailscale")]
    Wireguard,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SetupStage {
    #[default]
    PreWireguardExistingFlow,
    WireguardConfigGenerated,
    WireguardAppHandoffStarted,
    WireguardWaitingForImport,
    WireguardWaitingForActivation,
    WireguardVerifying,
    WireguardConnected,
    MoonlightSunshineReadyToSetup,
    SunshineCredentialsConfiguring,
    SunshineVerifying,
    MoonlightDetecting,
    MoonlightPairingStarted,
    MoonlightPinReceived,
    SunshinePinSubmitting,
    MoonlightSunshinePaired,
    SetupComplete,
    Failed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum WireGuardSetupMode {
    #[default]
    #[serde(
        alias = "wireguard_app_windows",
        alias = "wireguard_app_linux",
        alias = "wireguard_app_macos_manual"
    )]
    EmbeddedGotatun,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum WireGuardSetupStatus {
    #[default]
    NotStarted,
    ConfigGenerated,
    AppHandoffStarted,
    WaitingForUserImport,
    WaitingForUserActivation,
    Verifying,
    Connected,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SetupErrorState {
    pub code: String,
    pub message: String,
    pub stage: SetupStage,
    pub retryable: bool,
    pub details: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PostWireGuardSetupState {
    pub stage: SetupStage,
    pub wireguard_setup_mode: WireGuardSetupMode,
    pub wireguard_setup_status: WireGuardSetupStatus,
    pub current_instance_id: Option<u64>,
    pub wireguard_export_path: String,
    pub wireguard_config: String,
    pub wireguard_verified_host: String,
    pub wireguard_reachable_ports: Vec<u16>,
    pub sunshine_username: String,
    pub moonlight_host: String,
    pub paired: bool,
    pub setup_complete: bool,
    pub last_error: Option<SetupErrorState>,
}

impl Default for PostWireGuardSetupState {
    fn default() -> Self {
        Self {
            stage: SetupStage::PreWireguardExistingFlow,
            wireguard_setup_mode: WireGuardSetupMode::EmbeddedGotatun,
            wireguard_setup_status: WireGuardSetupStatus::NotStarted,
            current_instance_id: None,
            wireguard_export_path: String::new(),
            wireguard_config: String::new(),
            wireguard_verified_host: String::new(),
            wireguard_reachable_ports: Vec::new(),
            sunshine_username: String::new(),
            moonlight_host: String::new(),
            paired: false,
            setup_complete: false,
            last_error: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SunshineState {
    pub configured: bool,
    #[serde(default)]
    pub headless_edid_base64: String,
    #[serde(default = "default_edid_mode")]
    pub edid_mode: EdidMode,
    #[serde(default = "default_edid_refresh_rate_hz")]
    pub edid_refresh_rate_hz: u32,
    #[serde(default = "default_edid_source_label")]
    pub edid_source_label: String,
}

impl Default for SunshineState {
    fn default() -> Self {
        Self {
            configured: false,
            headless_edid_base64: String::new(),
            edid_mode: default_edid_mode(),
            edid_refresh_rate_hz: default_edid_refresh_rate_hz(),
            edid_source_label: default_edid_source_label(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EdidMode {
    AutoDetect,
    MacHardware,
    Manual,
}

fn default_edid_mode() -> EdidMode {
    EdidMode::AutoDetect
}

fn default_edid_refresh_rate_hz() -> u32 {
    60
}

fn default_edid_source_label() -> String {
    "Auto-Detected".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoonlightState {
    pub configured: bool,
    pub host_address: String,
    #[serde(default)]
    pub session_state: String,
    #[serde(default)]
    pub last_error: Option<String>,
}

impl Default for MoonlightState {
    fn default() -> Self {
        Self {
            configured: false,
            host_address: String::new(),
            session_state: "idle".to_string(),
            last_error: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoonlightPreferences {
    pub bitrate: u32,
    pub fps: u32,
    #[serde(default = "default_refresh_rate_mode")]
    pub refresh_rate_mode: String,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub display_output: Option<String>,
    #[serde(default)]
    pub aspect_ratio: Option<String>,
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
    #[serde(default)]
    pub show_input_debug_hud: u8,
}

impl Default for MoonlightPreferences {
    fn default() -> Self {
        Self {
            bitrate: 20000,
            fps: 60,
            refresh_rate_mode: default_refresh_rate_mode(),
            width: 1920,
            height: 1080,
            display_output: None,
            aspect_ratio: None,
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
            show_input_debug_hud: 0,
        }
    }
}

fn default_refresh_rate_mode() -> String {
    "60".to_string()
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
    #[serde(default = "default_true")]
    pub embedded_moonlight_pipeline_enabled: bool,
    #[serde(default)]
    pub embedded_moonlight_session_state: Option<String>,
    #[serde(default)]
    pub embedded_moonlight_last_error: Option<String>,
    #[serde(default)]
    pub embedded_moonlight_last_runtime_event: Option<String>,
    #[serde(default)]
    pub embedded_moonlight_runtime_connected: Option<bool>,
    #[serde(default)]
    pub embedded_moonlight_renderer_ready: Option<bool>,
    #[serde(default)]
    pub embedded_moonlight_video_session_active: Option<bool>,
    #[serde(default)]
    pub embedded_moonlight_video_frame_count: Option<u64>,
    #[serde(default)]
    pub embedded_moonlight_renderer_submitted_frame_count: Option<u64>,
    #[serde(default)]
    pub embedded_moonlight_renderer_dropped_frame_count: Option<u64>,
    #[serde(default)]
    pub embedded_moonlight_audio_sample_count: Option<u64>,
    #[serde(default)]
    pub embedded_moonlight_paired: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDisplayState {
    #[serde(default)]
    pub desired_profile_hash: String,
    #[serde(default)]
    pub installed_profile_hash: String,
    #[serde(default)]
    pub advertised_modes: Vec<crate::services::display_profile::DisplayModeSpec>,
    #[serde(default)]
    pub selected_mode: Option<crate::services::display_profile::DisplayModeSpec>,
    #[serde(default)]
    pub active_mode: Option<crate::services::display_profile::DisplayModeSpec>,
    #[serde(default)]
    pub output_name: Option<String>,
    #[serde(default)]
    pub applied_at: Option<String>,
    #[serde(default)]
    pub last_apply_error: Option<String>,
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
    #[serde(default)]
    pub connection_provider: ConnectionProvider,
    #[serde(default = "default_true")]
    pub embedded_moonlight_pipeline_enabled: bool,
    #[serde(default)]
    pub embedded_moonlight_host_id: String,
    #[serde(default)]
    pub embedded_moonlight_paired: bool,
    #[serde(default = "default_mic_device_id")]
    pub mic_device_id: String,
    #[serde(default = "default_mic_device_name")]
    pub mic_device_name: String,
    #[serde(default)]
    pub mic_quality_profile: MicQualityProfile,
    #[serde(default)]
    pub mic_forwarding_enabled: bool,
    #[serde(default = "default_true")]
    pub mic_auto_connect: bool,
    #[serde(default)]
    pub display: RemoteDisplayState,
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
    #[serde(default)]
    pub mic_receiver_installed: bool,
    pub wireguard_configured: bool,
    pub moonlight_configured: bool,
    pub awaiting_pair_pin: bool,
    pub pairing_completed: bool,
    #[serde(default)]
    pub post_provision_completed: bool,
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
            connection_provider: ConnectionProvider::default(),
            embedded_moonlight_pipeline_enabled: true,
            embedded_moonlight_host_id: format!("instance-{}", instance_id),
            embedded_moonlight_paired: false,
            mic_device_id: default_mic_device_id(),
            mic_device_name: default_mic_device_name(),
            mic_quality_profile: MicQualityProfile::Standard,
            mic_forwarding_enabled: false,
            mic_auto_connect: true,
            display: RemoteDisplayState::default(),
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
    #[serde(
        alias = "SelectingConnectionProvider",
        alias = "ConfiguringTailscale",
        alias = "TailscaleConfigGenerated"
    )]
    WireGuardConfigGenerated,
    WireGuardAppHandoffStarted,
    WireGuardWaitingForImport,
    WireGuardWaitingForActivation,
    WireGuardVerifying,
    #[serde(alias = "TailscaleConnected")]
    WireGuardConnected,
    MoonlightSunshineReadyToSetup,
    SunshineCredentialsConfiguring,
    SunshineVerifying,
    MoonlightDetecting,
    MoonlightPairingStarted,
    MoonlightPinReceived,
    SunshinePinSubmitting,
    MoonlightSunshinePaired,
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
pub struct SharedStorageState {
    pub settings: SharedStorageSettings,
    pub last_backup_started_at: Option<String>,
    pub last_backup_finished_at: Option<String>,
    pub last_backup_status: String,
    pub last_backup_error: Option<String>,
    pub last_backup_trigger: String,
}

impl Default for SharedStorageState {
    fn default() -> Self {
        Self {
            settings: SharedStorageSettings::default(),
            last_backup_started_at: None,
            last_backup_finished_at: None,
            last_backup_status: "never_run".to_string(),
            last_backup_error: None,
            last_backup_trigger: "none".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedStorageSettings {
    pub enabled: bool,
    pub backblaze_key_id: String,
    #[serde(default)]
    pub backblaze_application_key: String,
    pub bucket_name: String,
    pub remote_name: String,
    pub destination_prefix: String,
    #[serde(default)]
    pub crypt_password: Option<String>,
}

impl Default for SharedStorageSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            backblaze_key_id: String::new(),
            backblaze_application_key: String::new(),
            bucket_name: "noland".to_string(),
            remote_name: "b2".to_string(),
            destination_prefix: "vm-backup".to_string(),
            crypt_password: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedStorageSettingsUpdate {
    pub enabled: bool,
    pub backblaze_key_id: String,
    pub backblaze_application_key: String,
    pub bucket_name: String,
    pub remote_name: String,
    pub destination_prefix: String,
    #[serde(default)]
    pub crypt_password: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedStorageSettingsResponse {
    pub enabled: bool,
    pub backblaze_key_id: String,
    pub bucket_name: String,
    pub remote_name: String,
    pub destination_prefix: String,
    #[serde(default)]
    pub crypt_password_set: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupStatusResponse {
    pub last_backup_started_at: Option<String>,
    pub last_backup_finished_at: Option<String>,
    pub last_backup_status: String,
    pub last_backup_error: Option<String>,
    pub last_backup_trigger: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedStorageInstanceStatus {
    pub instance_id: u64,
    pub backup_running: bool,
    pub last_backup_started_at: Option<String>,
    pub last_backup_finished_at: Option<String>,
    pub last_backup_status: String,
    pub last_backup_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedStorageObjectEntry {
    pub path: String,
    pub name: String,
    pub parent_path: String,
    pub is_dir: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedStorageSyncSelectionRequest {
    pub selected_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SharedStorageCredentialState {
    #[serde(default)]
    pub profiles: HashMap<String, SharedStorageProfileSecret>,
    #[serde(default)]
    pub oauth_sessions: HashMap<String, SharedStorageOAuthSessionSecret>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedStorageProfileSecret {
    pub credentials: crate::models::application_bundle::StorageCredential,
    #[serde(default)]
    pub provider_fields: HashMap<String, String>,
    pub repository_key_hex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedStorageOAuthSessionSecret {
    pub credentials: crate::models::application_bundle::StorageCredential,
}

// ============================================================
// Bundle Index + Restore types
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleIndex {
    pub schema_version: u32,
    pub generated_at: String,
    pub instance_id: u64,
    pub snapshot_id: String,
    pub host: BundleHost,
    pub bundles: Vec<AppBundle>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleHost {
    pub username: String,
    pub home: String,
    pub os: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppBundle {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub bundle_type: String,
    pub confidence: f64,
    pub signals: Vec<String>,
    pub folder_bundles: Vec<FolderBundle>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderBundle {
    pub id: String,
    pub label: String,
    pub source: String,
    pub target: String,
    pub kind: String,
    pub default_selected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreRequest {
    pub bundle_id: String,
    pub folder_bundle_ids: Vec<String>,
    pub mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreDryRunResult {
    pub would_restore: Vec<RestoreDryRunItem>,
    pub total_files_estimate: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreDryRunItem {
    pub folder_bundle_id: String,
    pub label: String,
    pub source: String,
    pub target: String,
    pub kind: String,
    pub action: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreJob {
    pub job_id: String,
    pub instance_id: u64,
    pub bundle_id: String,
    pub mode: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub items: Vec<RestoreJobItem>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreJobItem {
    pub folder_bundle_id: String,
    pub label: String,
    pub source: String,
    pub target: String,
    pub kind: String,
    pub status: String,
    pub error: Option<String>,
}

// ============================================================
// Microphone Passthrough types
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceMicConfig {
    pub instance_id: u64,
    /// Whether a media session is currently active.
    pub enabled: bool,
    /// Persisted feature preference. This remains true between game streams.
    pub forwarding_enabled: bool,
    /// Automatically start/stop forwarding with the Moonlight session.
    pub auto_connect: bool,
    pub transport: String,
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u32,
    pub vm_wireguard_ip: String,
    pub rtp_port: u16,
    pub rtcp_port: u16,
    pub local_rtcp_port: u16,
    pub rtp_payload_type: u8,
    pub device_id: String,
    pub device_name: String,
    pub quality_profile: MicQualityProfile,
    pub session_id: Option<String>,
    pub session_token: Option<String>,
    pub ssrc: Option<u32>,
    pub last_enabled_at: Option<String>,
    pub last_disabled_at: Option<String>,
}

impl Default for InstanceMicConfig {
    fn default() -> Self {
        Self {
            instance_id: 0,
            enabled: false,
            forwarding_enabled: false,
            auto_connect: true,
            transport: "gstreamer_rtp_udp".to_string(),
            codec: "opus".to_string(),
            sample_rate: 48000,
            channels: 1,
            vm_wireguard_ip: String::new(),
            rtp_port: 0,
            rtcp_port: 0,
            local_rtcp_port: 0,
            rtp_payload_type: 111,
            device_id: default_mic_device_id(),
            device_name: default_mic_device_name(),
            quality_profile: MicQualityProfile::Standard,
            session_id: None,
            session_token: None,
            ssrc: None,
            last_enabled_at: None,
            last_disabled_at: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum MicQualityProfile {
    Standard,
    #[serde(alias = "lowlatency")]
    LowLatency,
    #[serde(alias = "highquality")]
    HighQuality,
}

impl Default for MicQualityProfile {
    fn default() -> Self {
        MicQualityProfile::Standard
    }
}

impl MicQualityProfile {
    pub fn bitrate_kbps(&self) -> u32 {
        match self {
            MicQualityProfile::Standard => 32,
            MicQualityProfile::LowLatency => 48,
            MicQualityProfile::HighQuality => 64,
        }
    }

    pub fn frame_ms(&self) -> u32 {
        match self {
            MicQualityProfile::Standard => 10,
            MicQualityProfile::LowLatency => 10,
            MicQualityProfile::HighQuality => 10,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceMicRuntimeStatus {
    pub enabled: bool,
    pub state: MicState,
    pub reconnect_count: u64,
    pub vm_agent_reachable: bool,
    pub device_ready: bool,
    pub receiving_audio: bool,
    pub transport: String,
    pub sample_rate: u32,
    pub channels: u32,
    pub bitrate_kbps: u32,
    pub frame_ms: u32,
    pub packet_loss_percent: f64,
    pub jitter_ms: f64,
    pub buffer_depth_ms: f64,
    pub last_packet_ms_ago: Option<u64>,
    pub pipewire_connected: bool,
    pub default_source: bool,
    pub muted: bool,
    pub sidecar_healthy: bool,
    pub capture_sample_rate: u32,
    pub capture_overruns: u64,
    pub ring_fill_ms: f64,
    pub appsrc_queue_ms: f64,
    pub opus_packets_sent: u64,
    pub bytes_sent: u64,
    pub error: Option<String>,
}

impl Default for InstanceMicRuntimeStatus {
    fn default() -> Self {
        Self {
            enabled: false,
            state: MicState::Disabled,
            reconnect_count: 0,
            vm_agent_reachable: false,
            device_ready: false,
            receiving_audio: false,
            transport: "gstreamer_rtp_udp".to_string(),
            sample_rate: 48000,
            channels: 1,
            bitrate_kbps: 32,
            frame_ms: 10,
            packet_loss_percent: 0.0,
            jitter_ms: 0.0,
            buffer_depth_ms: 0.0,
            last_packet_ms_ago: None,
            pipewire_connected: false,
            default_source: false,
            muted: false,
            sidecar_healthy: false,
            capture_sample_rate: 0,
            capture_overruns: 0,
            ring_fill_ms: 0.0,
            appsrc_queue_ms: 0.0,
            opus_packets_sent: 0,
            bytes_sent: 0,
            error: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MicState {
    Disabled,
    Starting,
    Connecting,
    Streaming,
    NoAudioDetected,
    WireguardDisconnected,
    VmAgentUnreachable,
    CloudMicMissing,
    PacketLossHigh,
    PipewireUnavailable,
    NoMicrophone,
    CaptureFailure,
    PipelineFailure,
    NetworkFailure,
    Reconnecting,
    Degraded,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MicSettingsUpdate {
    pub device_id: Option<String>,
    pub quality_profile: Option<MicQualityProfile>,
    pub forwarding_enabled: Option<bool>,
    pub auto_connect: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MicSessionResponse {
    pub session_id: String,
    pub session_token: String,
    pub ssrc: u32,
    pub vm_wireguard_ip: String,
    pub rtp_port: u16,
    pub rtcp_port: u16,
    pub local_rtcp_port: u16,
    pub rtp_payload_type: u8,
    pub sample_rate: u32,
    pub channels: u32,
    pub frame_ms: u32,
    pub bitrate_kbps: u32,
}

#[cfg(test)]
mod tests {
    use super::MicQualityProfile;

    #[test]
    fn mic_quality_profiles_use_frontend_camel_case() {
        assert_eq!(
            serde_json::to_string(&MicQualityProfile::LowLatency).unwrap(),
            r#""lowLatency""#
        );
        assert_eq!(
            serde_json::to_string(&MicQualityProfile::HighQuality).unwrap(),
            r#""highQuality""#
        );
        assert_eq!(
            serde_json::from_str::<MicQualityProfile>(r#""lowLatency""#).unwrap(),
            MicQualityProfile::LowLatency
        );
        assert_eq!(
            serde_json::from_str::<MicQualityProfile>(r#""highQuality""#).unwrap(),
            MicQualityProfile::HighQuality
        );
    }

    #[test]
    fn mic_quality_profiles_accept_legacy_lowercase_values() {
        assert_eq!(
            serde_json::from_str::<MicQualityProfile>(r#""lowlatency""#).unwrap(),
            MicQualityProfile::LowLatency
        );
        assert_eq!(
            serde_json::from_str::<MicQualityProfile>(r#""highquality""#).unwrap(),
            MicQualityProfile::HighQuality
        );
    }
}
