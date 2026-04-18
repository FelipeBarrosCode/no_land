use std::{env, time::Duration};

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub state_schema_version: u32,
    pub default_template_hash: String,
    pub min_host_reliability: f64,
    pub offers_search_limit: usize,
    pub vast_base_url: String,
    pub poll_interval: Duration,
    pub poll_max_attempts: usize,
    pub ssh_connect_probe_attempts: usize,
    pub ssh_connect_probe_interval: Duration,
    pub ssh_user: String,
    pub audio_target_user: String,
    pub audio_profile: String,
    pub audio_force_sink_override: bool,
    pub audio_sink_override: Option<String>,
    pub moonlight_download_url_windows: String,
    pub moonlight_download_url_macos: String,
    pub moonlight_download_url_linux: String,
    pub sunshine: SunshineDefaults,
    pub wireguard: WireGuardDefaults,
    pub scoring: OfferScoring,
    pub pairing: PairingDefaults,
}

#[derive(Debug, Clone)]
pub struct SunshineDefaults {
    pub audio_sink: String,
    pub av1_mode: i32,
    pub capture: String,
    pub encoder: String,
    pub fec_percentage: i32,
    pub hevc_mode: i32,
    pub nvenc_preset: i32,
    pub output_name: i32,
    pub ping_timeout: i32,
    pub port: u16,
    pub address: String,
    pub display: String,
    pub cpu_affinity: String,
    pub virtual_display_width: u32,
    pub virtual_display_height: u32,
    pub target_fps: u32,
}

impl SunshineDefaults {
    pub fn virtual_display_refresh_rate(&self) -> u32 {
        self.target_fps * 2
    }
}

#[derive(Debug, Clone)]
pub struct WireGuardDefaults {
    pub server_interface_name: String,
    pub server_tunnel_ip: String,
    pub client_tunnel_ip: String,
    pub listen_port: u16,
    pub stream_max_rate: String,
    pub tunnel_mtu: u16,
}

#[derive(Debug, Clone)]
pub struct OfferScoring {
    pub distance_weight: f64,
    pub price_weight: f64,
    pub vram_weight: f64,
}

#[derive(Debug, Clone)]
pub struct PairingDefaults {
    pub sunshine_pair_command: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        // Vast API reference:
        // https://docs.vast.ai/api-reference/introduction
        // https://docs.vast.ai/api-reference/search/search-offers
        // https://docs.vast.ai/api-reference/instances/create-instance
        // https://docs.vast.ai/documentation/instances/connect/ssh
        // API key setup flow:
        // https://cloud.vast.ai/cli/
        Self {
            state_schema_version: 1,
            default_template_hash: env::var("NOLAND_TEMPLATE_HASH")
                .unwrap_or_else(|_| "2a62a7d5089a50a5ad89a9480f540d25".to_string()),
            min_host_reliability: env::var("NOLAND_MIN_RELIABILITY")
                .ok()
                .and_then(|value| value.parse::<f64>().ok())
                .unwrap_or(0.85),
            offers_search_limit: env::var("NOLAND_OFFERS_SEARCH_LIMIT")
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(500),
            vast_base_url: env::var("NOLAND_VAST_BASE_URL")
                .unwrap_or_else(|_| "https://console.vast.ai".to_string()),
            poll_interval: Duration::from_secs(60),
            poll_max_attempts: 120, // 120 minutes max for slow-boot high-RAM machines
            ssh_connect_probe_attempts: 60, // More lenient SSH probing
            ssh_connect_probe_interval: Duration::from_secs(30), // Check every 30s instead of 60s
            ssh_user: env::var("NOLAND_INSTANCE_SSH_USER").unwrap_or_else(|_| "root".to_string()),
            audio_target_user: env::var("NOLAND_AUDIO_TARGET_USER")
                .unwrap_or_else(|_| "user".to_string()),
            audio_profile: env::var("NOLAND_AUDIO_PROFILE")
                .unwrap_or_else(|_| "aggressive".to_string()),
            audio_force_sink_override: env_bool("NOLAND_AUDIO_FORCE_SINK_OVERRIDE"),
            audio_sink_override: env::var("NOLAND_AUDIO_SINK_OVERRIDE")
                .ok()
                .and_then(|value| {
                    let trimmed = value.trim().to_string();
                    if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed)
                    }
                }),
            moonlight_download_url_windows:
                "https://github.com/moonlight-stream/moonlight-qt/releases".to_string(),
            moonlight_download_url_macos:
                "https://github.com/moonlight-stream/moonlight-qt/releases".to_string(),
            moonlight_download_url_linux:
                "https://github.com/moonlight-stream/moonlight-qt/releases".to_string(),
            sunshine: SunshineDefaults {
                audio_sink: "sunshine_audio".to_string(),
                av1_mode: 1,
                capture: "nvfbc".to_string(),
                encoder: "nvenc".to_string(),
                fec_percentage: 30,
                hevc_mode: 0,
                nvenc_preset: 4,
                output_name: 0,
                ping_timeout: 30000,
                port: 47990,
                address: "0.0.0.0".to_string(),
                display: ":0".to_string(),
                cpu_affinity: "2-5".to_string(),
                virtual_display_width: 1920,
                virtual_display_height: 1080,
                target_fps: 60,
            },
            wireguard: WireGuardDefaults {
                server_interface_name: "wg0".to_string(),
                server_tunnel_ip: "10.77.0.1/24".to_string(),
                client_tunnel_ip: "10.77.0.2/32".to_string(),
                listen_port: 51820,
                stream_max_rate: "80mbit".to_string(),
                tunnel_mtu: 1380,
            },
            scoring: OfferScoring {
                distance_weight: 0.7,
                price_weight: 0.2,
                vram_weight: 0.1,
            },
            pairing: PairingDefaults {
                sunshine_pair_command:
                    "printf '%s\n' '{pin}' | sunshine-cli pair || sunshine --pair-pin '{pin}'"
                        .to_string(),
            },
        }
    }
}

fn env_bool(key: &str) -> bool {
    env::var(key)
        .ok()
        .map(|value| {
            let normalized = value.trim().to_ascii_lowercase();
            matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(false)
}
