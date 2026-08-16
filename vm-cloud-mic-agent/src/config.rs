use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const RTP_PAYLOAD_TYPE: u8 = 111;
pub const RTP_CLOCK_RATE: u32 = 48_000;
pub const MIN_JITTER_MS: u32 = 10;
pub const MAX_JITTER_MS: u32 = 60;
pub const MAX_DATAGRAM_BYTES: usize = 1_200;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML parse error: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("invalid receiver configuration: {0}")]
    Validation(String),
}

/// Receiver configuration. The SSH control plane owns allocation and writes this
/// file before restarting the independently managed receiver service.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ReceiverConfig {
    #[serde(default)]
    pub network: NetworkConfig,
    #[serde(default)]
    pub audio: AudioConfig,
    #[serde(default)]
    pub jitter: JitterConfig,
    #[serde(default)]
    pub session: SessionConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NetworkConfig {
    #[serde(default = "default_bind_address")]
    pub bind_address: String,
    #[serde(default = "default_rtp_port", alias = "port")]
    pub rtp_port: u16,
    #[serde(default = "default_rtcp_port")]
    pub rtcp_port: u16,
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
    #[serde(default = "default_pw_sink_name", alias = "pipewire_node_name")]
    pub pipewire_sink_name: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct JitterConfig {
    #[serde(default = "default_jitter_initial")]
    pub initial_ms: u32,
    #[serde(default = "default_jitter_min")]
    pub minimum_ms: u32,
    #[serde(default = "default_jitter_max")]
    pub maximum_ms: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SessionConfig {
    #[serde(default = "default_session_id")]
    pub session_id: String,
    #[serde(default)]
    pub expected_peer_ip: Option<String>,
    #[serde(default = "default_client_rtcp_port")]
    pub client_rtcp_port: u16,
    #[serde(default)]
    pub expected_ssrc: Option<u32>,
}

fn default_bind_address() -> String {
    "10.77.0.1".to_string()
}
fn default_rtp_port() -> u16 {
    48_200
}
fn default_rtcp_port() -> u16 {
    48_201
}
fn default_interface() -> String {
    "wg0".to_string()
}
fn default_max_packet_size() -> usize {
    MAX_DATAGRAM_BYTES
}
fn default_recv_buf() -> usize {
    512 * 1024
}
fn default_sample_rate() -> u32 {
    RTP_CLOCK_RATE
}
fn default_channels() -> u8 {
    1
}
fn default_frame_duration_ms() -> u32 {
    10
}
fn default_pw_sink_name() -> String {
    "noland_mic_sink".to_string()
}
fn default_jitter_initial() -> u32 {
    20
}
fn default_jitter_min() -> u32 {
    MIN_JITTER_MS
}
fn default_jitter_max() -> u32 {
    MAX_JITTER_MS
}
fn default_session_id() -> String {
    "unallocated".to_string()
}
fn default_client_rtcp_port() -> u16 {
    48_202
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            bind_address: default_bind_address(),
            rtp_port: default_rtp_port(),
            rtcp_port: default_rtcp_port(),
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
            pipewire_sink_name: default_pw_sink_name(),
        }
    }
}

impl Default for JitterConfig {
    fn default() -> Self {
        Self {
            initial_ms: default_jitter_initial(),
            minimum_ms: default_jitter_min(),
            maximum_ms: default_jitter_max(),
        }
    }
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            session_id: default_session_id(),
            expected_peer_ip: None,
            client_rtcp_port: default_client_rtcp_port(),
            expected_ssrc: None,
        }
    }
}

impl ReceiverConfig {
    pub fn load(path: &str) -> Result<Self, ConfigError> {
        let content = std::fs::read_to_string(path)?;
        let config: ReceiverConfig = toml::from_str(&content)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        let bind_ip = parse_ip("network.bind_address", &self.network.bind_address)?;
        if bind_ip.is_unspecified() || bind_ip.is_multicast() {
            return validation("network.bind_address must be a unicast WireGuard address");
        }
        if self.network.interface.trim().is_empty() {
            return validation("network.interface must not be empty");
        }
        if self.network.rtp_port == self.network.rtcp_port {
            return validation("network.rtp_port and network.rtcp_port must differ");
        }
        if self.network.maximum_packet_size < 12
            || self.network.maximum_packet_size > MAX_DATAGRAM_BYTES
        {
            return validation(format!(
                "network.maximum_packet_size must be between 12 and {MAX_DATAGRAM_BYTES}"
            ));
        }
        if self.network.recv_buffer_bytes < self.network.maximum_packet_size {
            return validation(
                "network.recv_buffer_bytes must be at least network.maximum_packet_size",
            );
        }
        if self.audio.sample_rate != RTP_CLOCK_RATE || self.audio.channels != 1 {
            return validation("audio must be 48000 Hz mono");
        }
        if !matches!(self.audio.frame_duration_ms, 10 | 20) {
            return validation("audio.frame_duration_ms must be 10 or 20");
        }
        if self.audio.pipewire_sink_name != "noland_mic_sink" {
            return validation("audio.pipewire_sink_name must be noland_mic_sink");
        }
        if self.jitter.minimum_ms < MIN_JITTER_MS
            || self.jitter.maximum_ms > MAX_JITTER_MS
            || self.jitter.minimum_ms > self.jitter.maximum_ms
            || self.jitter.initial_ms < self.jitter.minimum_ms
            || self.jitter.initial_ms > self.jitter.maximum_ms
        {
            return validation(format!(
                "jitter bounds must satisfy {MIN_JITTER_MS} <= minimum <= initial <= maximum <= {MAX_JITTER_MS} ms"
            ));
        }
        if self.session.session_id.trim().is_empty() {
            return validation("session.session_id must not be empty");
        }
        if self.session.client_rtcp_port == 0 {
            return validation("session.client_rtcp_port must be non-zero");
        }
        if let Some(peer) = &self.session.expected_peer_ip {
            let peer_ip = parse_ip("session.expected_peer_ip", peer)?;
            if peer_ip.is_unspecified() || peer_ip.is_multicast() {
                return validation("session.expected_peer_ip must be a unicast address");
            }
            if peer_ip.is_ipv4() != bind_ip.is_ipv4() {
                return validation(
                    "session.expected_peer_ip and network.bind_address must use the same IP family",
                );
            }
        }
        Ok(())
    }
}

fn parse_ip(field: &str, value: &str) -> Result<IpAddr, ConfigError> {
    value
        .parse()
        .map_err(|_| ConfigError::Validation(format!("{field} must be an IP address")))
}

fn validation<T>(message: impl Into<String>) -> Result<T, ConfigError> {
    Err(ConfigError::Validation(message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_defaults_are_valid() {
        ReceiverConfig::default().validate().unwrap();
    }

    #[test]
    fn accepts_legacy_port_name() {
        let config: ReceiverConfig = toml::from_str(
            r#"
            [network]
            port = 49000
            rtcp_port = 49001
            "#,
        )
        .unwrap();
        assert_eq!(config.network.rtp_port, 49_000);
        config.validate().unwrap();
    }

    #[test]
    fn rejects_unsafe_jitter_and_datagram_values() {
        let mut config = ReceiverConfig::default();
        config.jitter.initial_ms = 9;
        assert!(config.validate().is_err());

        config = ReceiverConfig::default();
        config.network.maximum_packet_size = 1_201;
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_same_ports_and_non_ip_peer() {
        let mut config = ReceiverConfig::default();
        config.network.rtcp_port = config.network.rtp_port;
        assert!(config.validate().is_err());

        config = ReceiverConfig::default();
        config.session.expected_peer_ip = Some("wg-peer".to_string());
        assert!(config.validate().is_err());
    }
}
