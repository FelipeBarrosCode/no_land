use std::{env, time::Duration};

#[derive(Clone)]
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
    pub igdb: IgdbConfig,

    pub sunshine: SunshineDefaults,
    pub wireguard: WireGuardDefaults,
    pub scoring: OfferScoring,
}

#[derive(Clone, Default)]
pub struct IgdbConfig {
    pub twitch_client_id: Option<String>,
    pub twitch_client_secret: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SunshineDefaults {
    pub av1_mode: i32,
    pub bind_address: String,
    pub cpu_affinity: String,
    pub csrf_allowed_origins: String,
    pub encoder: String,
    pub fec_percentage: i32,
    pub hevc_mode: i32,
    pub minimum_fps_target: u32,
    pub nvenc_latency_over_power: String,
    pub nvenc_preset: i32,
    pub ping_timeout: i32,
    pub port: u16,
}

#[derive(Debug, Clone)]
pub struct WireGuardDefaults {
    pub server_interface_name: String,
    pub server_tunnel_ip: String,
    pub client_tunnel_ip: String,
    pub listen_port: u16,
    pub client_listen_port: u16,
    pub tunnel_mtu: u16,
    pub persistent_keepalive_secs: u16,
    pub qos_mode: String,
    pub qos_bandwidth_mbit: u32,
    pub qos_diffserv_profile: String,
    pub dscp_enabled: bool,
}

#[derive(Debug, Clone)]
pub struct OfferScoring {
    pub distance_weight: f64,
    pub price_weight: f64,
    pub vram_weight: f64,
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
            state_schema_version: 2,
            default_template_hash: env::var("NOLAND_TEMPLATE_HASH")
                .unwrap_or_else(|_| "566868bff8b15eef891ee706acbbb5e5".to_string()),
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
            igdb: IgdbConfig {
                twitch_client_id: non_empty_env("NOLAND_TWITCH_CLIENT_ID")
                    .or_else(|| non_empty_env("TWITCH_CLIENT_ID")),
                twitch_client_secret: non_empty_env("NOLAND_TWITCH_CLIENT_SECRET")
                    .or_else(|| non_empty_env("TWITCH_CLIENT_SECRET")),
            },

            sunshine: SunshineDefaults {
                av1_mode: 1,
                bind_address: env::var("NOLAND_SUNSHINE_BIND_ADDRESS").unwrap_or_default(),
                cpu_affinity: "2-5".to_string(),
                csrf_allowed_origins: env::var("NOLAND_SUNSHINE_CSRF_ALLOWED_ORIGINS")
                    .unwrap_or_else(|_| {
                        "https://localhost:47990,https://127.0.0.1:47990,https://10.77.0.1:47990"
                            .to_string()
                    }),
                encoder: "nvenc".to_string(),
                fec_percentage: 25,
                hevc_mode: 0,
                minimum_fps_target: 60,
                nvenc_latency_over_power: "enabled".to_string(),
                nvenc_preset: 3,
                ping_timeout: 30000,
                port: 47989,
            },
            wireguard: WireGuardDefaults {
                server_interface_name: "wg0".to_string(),
                server_tunnel_ip: "10.77.0.1/24".to_string(),
                client_tunnel_ip: "10.77.0.2/32".to_string(),
                listen_port: 51820,
                client_listen_port: env::var("NOLAND_WIREGUARD_CLIENT_LISTEN_PORT")
                    .ok()
                    .and_then(|value| value.parse::<u16>().ok())
                    .filter(|value| *value != 0)
                    .unwrap_or(51821),
                tunnel_mtu: 1280,
                persistent_keepalive_secs: 25,
                qos_mode: env::var("NOLAND_WIREGUARD_QOS_MODE")
                    .unwrap_or_else(|_| "cake".to_string()),
                qos_bandwidth_mbit: env::var("NOLAND_WIREGUARD_QOS_BANDWIDTH_MBIT")
                    .ok()
                    .and_then(|value| value.parse::<u32>().ok())
                    .unwrap_or(0),
                qos_diffserv_profile: env::var("NOLAND_WIREGUARD_QOS_DIFFSERV")
                    .unwrap_or_else(|_| "diffserv4".to_string()),
                dscp_enabled: env_bool("NOLAND_WIREGUARD_DSCP_ENABLED")
                    || env::var("NOLAND_WIREGUARD_DSCP_ENABLED").is_err(),
            },
            scoring: OfferScoring {
                distance_weight: 0.7,
                price_weight: 0.2,
                vram_weight: 0.1,
            },
        }
    }
}

fn non_empty_env(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
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
