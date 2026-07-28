pub mod capture;
pub mod device_list;
pub mod encoder;
pub mod transport;

use std::time::Instant;

use crossbeam_channel::{bounded, Receiver};
use tokio::sync::oneshot;
use tracing::{error, info, warn};

use crate::errors::{AppError, AppResult};
use crate::mic_client::transport::MicrophoneTransport;
use crate::models::app_state::MicQualityProfile;

/// A captured chunk of PCM audio from the microphone.
#[derive(Debug, Clone)]
pub struct CaptureChunk {
    pub timestamp: Instant,
    pub samples: Vec<f32>,
}

/// Configuration for the microphone client pipeline.
#[derive(Debug, Clone)]
pub struct MicClientConfig {
    pub device_id: Option<String>,
    pub quality_profile: MicQualityProfile,
    pub session_id: u64,
    pub session_secret: Vec<u8>,
    pub ssrc: u32,
    pub remote_addr: String,
}

/// Handle to a running microphone client pipeline.
pub struct MicClientHandle {
    stop_tx: Option<oneshot::Sender<()>>,
}

impl MicClientHandle {
    pub fn stop(&mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
    }
}

impl Drop for MicClientHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Start the microphone capture → encode → transport pipeline.
pub fn start_pipeline(config: MicClientConfig) -> AppResult<MicClientHandle> {
    let (capture_tx, capture_rx) = bounded::<CaptureChunk>(64);

    let _capture =
        capture::MicCaptureDevice::open_and_start(config.device_id.as_deref(), capture_tx)?;

    let (stop_tx, stop_rx) = oneshot::channel::<()>();

    let session_id = config.session_id;
    let ssrc = config.ssrc;
    let remote_addr = config.remote_addr.clone();

    std::thread::Builder::new()
        .name("noland-mic-encoder".into())
        .spawn(move || {
            run_encoder_loop(config, capture_rx, stop_rx);
        })
        .map_err(|e| AppError::Command(format!("Failed to spawn encoder thread: {e}")))?;

    info!(
        session_id = session_id,
        ssrc = ssrc,
        remote_addr = %remote_addr,
        "Microphone client pipeline started"
    );

    Ok(MicClientHandle {
        stop_tx: Some(stop_tx),
    })
}

fn run_encoder_loop(
    config: MicClientConfig,
    rx: Receiver<CaptureChunk>,
    mut stop_rx: oneshot::Receiver<()>,
) {
    let encoder =
        match encoder::OpusMicEncoder::new(config.quality_profile.bitrate_kbps() as i32 * 1000) {
            Ok(enc) => enc,
            Err(e) => {
                error!("Failed to create Opus encoder: {e}");
                return;
            }
        };

    let mut transport = match transport::NolandUdpV1Transport::connect(
        &config.remote_addr,
        config.session_id,
        config.session_secret,
        config.ssrc,
    ) {
        Ok(t) => t,
        Err(e) => {
            error!(addr = %config.remote_addr, "Failed to connect mic transport: {e}");
            return;
        }
    };

    let mut sequence: u16 = 0;
    let mut timestamp: u32 = 0;
    let samples_per_frame = 480u32;

    loop {
        if stop_rx.try_recv().is_ok() {
            info!("Mic encoder loop received stop signal");
            let buf = encoder.encode_silence();
            let _ = transport.send_frame(
                sequence,
                timestamp,
                noland_mic_protocol::flags::END_OF_STREAM,
                &buf,
            );
            return;
        }

        match rx.recv() {
            Ok(chunk) => {
                let num_samples = chunk.samples.len().min(480);
                let samples = if num_samples < 480 {
                    let mut padded = vec![0.0f32; 480];
                    padded[..num_samples].copy_from_slice(&chunk.samples[..num_samples]);
                    padded
                } else {
                    chunk.samples
                };

                let opus_buf = encoder
                    .encode(&samples[..480])
                    .unwrap_or_else(|_| encoder.encode_silence());

                let mut flags = noland_mic_protocol::flags::OPUS_FRAME;
                if matches!(config.quality_profile, MicQualityProfile::HighQuality) {
                    flags |= noland_mic_protocol::flags::FEC_ENABLED;
                }

                if let Err(e) = transport.send_frame(sequence, timestamp, flags, &opus_buf) {
                    warn!(sequence, "Mic transport send failed: {e}");
                }

                sequence = sequence.wrapping_add(1);
                timestamp = timestamp.wrapping_add(samples_per_frame);
            }
            Err(_) => {
                info!("Mic capture channel closed, stopping encoder loop");
                let buf = encoder.encode_silence();
                let _ = transport.send_frame(
                    sequence,
                    timestamp,
                    noland_mic_protocol::flags::END_OF_STREAM,
                    &buf,
                );
                return;
            }
        }
    }
}
