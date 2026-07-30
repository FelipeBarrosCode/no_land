use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML parse error: {0}")]
    Toml(#[from] toml::de::Error),
}

/// Receiver configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReceiverConfig {
    #[serde(default)]
    pub network: NetworkConfig,
    #[serde(default)]
    pub audio: AudioConfig,
    #[serde(default)]
    pub jitter: JitterConfig,
    #[serde(default)]
    pub security: SecurityConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NetworkConfig {
    #[serde(default = "default_bind_address")]
    pub bind_address: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_interface")]
    pub interface: String,
    #[serde(default = "default_max_packet_size")]
    pub maximum_packet_size: usize,
    #[serde(default = "default_recv_buf")]
    pub recv_buffer_bytes: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AudioConfig {
    #[serde(default = "default_sample_rate")]
    pub sample_rate: u32,
    #[serde(default = "default_channels")]
    pub channels: u8,
    #[serde(default = "default_frame_duration_ms")]
    pub frame_duration_ms: u32,
    #[serde(default = "default_pw_node_name")]
    pub pipewire_node_name: String,
    #[serde(default = "default_pw_description")]
    pub pipewire_description: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct JitterConfig {
    #[serde(default = "default_jitter_initial")]
    pub initial_ms: f64,
    #[serde(default = "default_jitter_min")]
    pub minimum_ms: f64,
    #[serde(default = "default_jitter_max")]
    pub maximum_ms: f64,
    #[serde(default = "default_reorder_window")]
    pub reorder_window_packets: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SecurityConfig {
    #[serde(default = "default_false")]
    pub require_active_session: bool,
    #[serde(default = "default_false")]
    pub require_packet_authentication: bool,
    #[serde(default = "default_session_timeout")]
    pub session_timeout_seconds: u64,
}

fn default_bind_address() -> String {
    "10.77.0.1".to_string()
}
fn default_port() -> u16 {
    48200
}
fn default_interface() -> String {
    "wg0".to_string()
}
fn default_max_packet_size() -> usize {
    1200
}
fn default_recv_buf() -> usize {
    512 * 1024
}
fn default_sample_rate() -> u32 {
    48000
}
fn default_channels() -> u8 {
    1
}
fn default_frame_duration_ms() -> u32 {
    10
}
fn default_pw_node_name() -> String {
    "noland_remote_microphone".to_string()
}
fn default_pw_description() -> String {
    "Noland Remote Microphone".to_string()
}
fn default_jitter_initial() -> f64 {
    25.0
}
fn default_jitter_min() -> f64 {
    15.0
}
fn default_jitter_max() -> f64 {
    60.0
}
fn default_reorder_window() -> usize {
    64
}
fn default_false() -> bool {
    false
}
fn default_session_timeout() -> u64 {
    5
}

impl Default for ReceiverConfig {
    fn default() -> Self {
        Self {
            network: NetworkConfig::default(),
            audio: AudioConfig::default(),
            jitter: JitterConfig::default(),
            security: SecurityConfig::default(),
        }
    }
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            bind_address: default_bind_address(),
            port: default_port(),
            interface: default_interface(),
            maximum_packet_size: default_max_packet_size(),
            recv_buffer_bytes: default_recv_buf(),
        }
    }
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            sample_rate: default_sample_rate(),
            channels: default_channels(),
            frame_duration_ms: default_frame_duration_ms(),
            pipewire_node_name: default_pw_node_name(),
            pipewire_description: default_pw_description(),
        }
    }
}

impl Default for JitterConfig {
    fn default() -> Self {
        Self {
            initial_ms: default_jitter_initial(),
            minimum_ms: default_jitter_min(),
            maximum_ms: default_jitter_max(),
            reorder_window_packets: default_reorder_window(),
        }
    }
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            require_active_session: default_false(),
            require_packet_authentication: default_false(),
            session_timeout_seconds: default_session_timeout(),
        }
    }
}

impl ReceiverConfig {
    pub fn load(path: &str) -> Result<Self, ConfigError> {
        let content = std::fs::read_to_string(path)?;
        let config: ReceiverConfig = toml::from_str(&content)?;
        Ok(config)
    }
}
