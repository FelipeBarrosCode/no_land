use crate::capture::SharedInput;
use crate::metrics::Metrics;
use crate::protocol::SessionConfig;
use serde::Serialize;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

pub const RTP_PAYLOAD_TYPE: u32 = 111;
pub const MAX_RTP_PAYLOAD_BYTES: u32 = 1_200;
const UNSUPPORTED_MESSAGE: &str = "microphone passthrough is not supported on Windows ARM64 because upstream GStreamer MSVC ARM64 packages are unavailable";

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RtpOffsets {
    pub ssrc: u32,
    pub sequence: u16,
    pub timestamp: u32,
}

pub struct PipelineSession {
    pub rtp_offsets: RtpOffsets,
    pub webrtc_dsp_enabled: bool,
}

impl PipelineSession {
    pub fn start(
        _config: &SessionConfig,
        _input: SharedInput,
        _muted: Arc<AtomicBool>,
        _metrics: Arc<Metrics>,
    ) -> Result<Self, String> {
        Err(UNSUPPORTED_MESSAGE.to_string())
    }

    pub fn set_bitrate(&self, _bitrate: u32) -> Result<(), String> {
        Err(UNSUPPORTED_MESSAGE.to_string())
    }

    pub fn poll_error(&self) -> Option<String> {
        Some(UNSUPPORTED_MESSAGE.to_string())
    }

    pub fn stop(&mut self) {}
}
