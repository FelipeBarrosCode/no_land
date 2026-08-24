use crate::capture::{DeviceInfo, SourceKind};
use crate::metrics::MetricsSnapshot;
use crate::state::Status;
use serde::{Deserialize, Serialize};
use serde_json::Value;

fn default_bitrate() -> u32 {
    32_000
}
fn default_frame_ms() -> u32 {
    10
}
fn default_loss() -> u32 {
    5
}
fn default_rtcp_listen_port() -> u16 {
    0
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfig {
    pub session_id: String,
    pub host: String,
    pub rtp_port: u16,
    /// Separate destination RTCP port. RTP/RTCP mux is intentionally deferred.
    #[serde(default)]
    pub rtcp_port: Option<u16>,
    /// Separate local RTCP receive port. Zero derives `rtpPort + 2`.
    #[serde(default = "default_rtcp_listen_port")]
    pub rtcp_listen_port: u16,
    #[serde(default = "default_bitrate")]
    pub bitrate: u32,
    #[serde(default = "default_frame_ms")]
    pub frame_ms: u32,
    #[serde(default)]
    pub fec: bool,
    #[serde(default = "default_loss")]
    pub packet_loss_percent: u32,
    #[serde(default)]
    pub dtx: bool,
    #[serde(default)]
    pub ssrc: Option<u32>,
    #[serde(default)]
    pub sequence_offset: Option<u16>,
    #[serde(default)]
    pub timestamp_offset: Option<u32>,
    #[serde(default)]
    pub source: SourceKind,
}

impl SessionConfig {
    pub fn resolved_rtcp_port(&self) -> Result<u16, String> {
        self.rtcp_port
            .or_else(|| self.rtp_port.checked_add(1))
            .ok_or_else(|| "rtcpPort is required when rtpPort is 65535".to_string())
    }

    pub fn resolved_rtcp_listen_port(&self) -> Result<u16, String> {
        if self.rtcp_listen_port != 0 {
            return Ok(self.rtcp_listen_port);
        }
        self.rtp_port.checked_add(2).ok_or_else(|| {
            "rtcpListenPort is required when rtpPort cannot derive a separate port".to_string()
        })
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.session_id.trim().is_empty() {
            return Err("sessionId must not be empty".to_string());
        }
        if self.host.trim().is_empty() {
            return Err("host must not be empty".to_string());
        }
        if self.rtp_port == 0 {
            return Err("rtpPort must be non-zero".to_string());
        }
        self.resolved_rtcp_port()?;
        self.resolved_rtcp_listen_port()?;
        if self.frame_ms != 10 {
            return Err("only 10 ms Opus frames are supported".to_string());
        }
        if !(6_000..=128_000).contains(&self.bitrate) {
            return Err("bitrate must be between 6000 and 128000 bits/s".to_string());
        }
        if self.packet_loss_percent > 100 {
            return Err("packetLossPercent must be between 0 and 100".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
pub struct Request {
    #[serde(default)]
    pub id: Value,
    #[serde(flatten)]
    pub command: Command,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "command", rename_all = "camelCase")]
pub enum Command {
    ListDevices,
    GetStatus,
    SelectDevice {
        #[serde(rename = "deviceId")]
        device_id: Option<String>,
    },
    StartSession {
        config: SessionConfig,
    },
    StopSession,
    Mute,
    Unmute,
    SetBitrate {
        bitrate: u32,
    },
    GetMetrics,
    Shutdown,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Output {
    Response {
        id: Value,
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<ResponseResult>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    Event {
        event: String,
        data: Value,
    },
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum ResponseResult {
    Devices { devices: Vec<DeviceInfo> },
    Status(Status),
    Metrics(MetricsSnapshot),
    SessionConfig(SessionConfig),
    Ack { acknowledged: bool },
}

impl Output {
    pub fn success(id: Value, result: ResponseResult) -> Self {
        Self::Response {
            id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Value, error: impl Into<String>) -> Self {
        Self::Response {
            id,
            ok: false,
            result: None,
            error: Some(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_config_round_trips() {
        let config = SessionConfig {
            session_id: "session-test".into(),
            host: "127.0.0.1".into(),
            rtp_port: 5004,
            rtcp_port: Some(5005),
            rtcp_listen_port: 5006,
            bitrate: 24_000,
            frame_ms: 10,
            fec: true,
            packet_loss_percent: 8,
            dtx: true,
            ssrc: Some(7),
            sequence_offset: Some(11),
            timestamp_offset: Some(13),
            source: SourceKind::Sine,
        };
        let json = serde_json::to_string(&config).unwrap();
        let decoded: SessionConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, config);
        assert!(decoded.validate().is_ok());
    }

    #[test]
    fn request_id_is_preserved_as_json_value() {
        let request: Request = serde_json::from_str(r#"{"id":"abc","command":"mute"}"#).unwrap();
        assert_eq!(request.id, Value::String("abc".into()));
        assert!(matches!(request.command, Command::Mute));
    }
}
