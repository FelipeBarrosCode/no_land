use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MicrophoneDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MicrophoneState {
    Stopped,
    Starting,
    Running,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MicrophoneStatus {
    pub state: MicrophoneState,
    pub device_id: Option<String>,
    pub device_name: Option<String>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
    pub destination: Option<String>,
    pub dropped_samples: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum MicrophoneError {
    #[error("No input device found")]
    NoInputDevice,
    #[error("Permission denied")]
    PermissionDenied,
    #[error("Device unavailable")]
    DeviceUnavailable,
    #[error("Unsupported sample format: {0}")]
    UnsupportedSampleFormat(String),
    #[error("Failed to build stream: {0}")]
    StreamBuildFailed(String),
    #[error("Failed to start stream: {0}")]
    StreamStartFailed(String),
    #[error("GStreamer not found")]
    GStreamerNotFound,
    #[error("Failed to spawn GStreamer: {0}")]
    GStreamerSpawnFailed(String),
    #[error("GStreamer exited unexpectedly")]
    GStreamerExited,
    #[error("Audio pipe closed")]
    AudioPipeClosed,
    #[error("Already running")]
    AlreadyRunning,
    #[error("Not running")]
    NotRunning,
    #[error("Internal error: {0}")]
    Internal(String),
}
