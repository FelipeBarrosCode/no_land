//! Measured stream windows, independent of overlay visibility.
use serde::Serialize;

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PerformanceStatistics {
    pub incoming_fps: f64,
    pub decoded_fps: Option<f64>,
    /// Submission to the platform renderer, not physical display scanout.
    pub submitted_fps: f64,
    pub video_mbps: f64,
    /// Gaps before the renderer; includes core queue discards, not packet loss.
    pub missing_frames_percent: f64,
    pub host_processing_ms: Option<f64>,
    pub reassembly_ms: Option<f64>,
    pub decode_ms: Option<f64>,
    pub render_queue_ms: Option<f64>,
    pub width: u32,
    pub height: u32,
    pub codec: String,
    pub samples: u32,
}

fn measured(value: f64) -> Option<f64> {
    (value.is_finite() && value >= 0.0).then_some(value)
}

pub(super) unsafe fn read(runtime: *mut super::native::nl_runtime_t) -> PerformanceStatistics {
    let mut raw = super::native::nl_performance_stats_t::default();
    unsafe { super::native::nl_runtime_read_performance(runtime, &mut raw) };
    PerformanceStatistics {
        incoming_fps: raw.incoming_fps,
        decoded_fps: measured(raw.decoded_fps),
        submitted_fps: raw.submitted_fps,
        video_mbps: raw.video_mbps,
        missing_frames_percent: raw.missing_frames_percent,
        host_processing_ms: measured(raw.host_processing_ms),
        reassembly_ms: measured(raw.reassembly_ms),
        decode_ms: measured(raw.decode_ms),
        render_queue_ms: measured(raw.render_queue_ms),
        width: raw.width,
        height: raw.height,
        codec: if raw.video_format & 0xf000 != 0 {
            "AV1"
        } else if raw.video_format & 0x0f00 != 0 {
            "HEVC"
        } else if raw.video_format != 0 {
            "H.264"
        } else {
            "—"
        }
        .into(),
        samples: raw.samples,
    }
}
