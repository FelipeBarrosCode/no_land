use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Stream, StreamConfig};
use crossbeam_channel::Sender;
use tracing::{info, warn};

use crate::errors::{AppError, AppResult};

use super::CaptureChunk;

/// Wrapper around a cpal input stream that pushes captured audio into a channel.
pub struct MicCaptureDevice {
    stream: Stream,
    name: String,
}

impl MicCaptureDevice {
    /// Open a recording device and start capturing immediately.
    ///
    /// Captured audio chunks (480-sample float32 mono at 48 kHz) are pushed
    /// into `tx`. The stream runs on cpal's internal audio thread.
    pub fn open_and_start(
        device_name: Option<&str>,
        tx: Sender<CaptureChunk>,
    ) -> AppResult<Self> {
        let host = cpal::default_host();

        let device = match device_name {
            Some(name) => {
                let devices = host.input_devices().map_err(|e| {
                    AppError::Command(format!("Failed to enumerate input devices: {e}"))
                })?;

                let mut found = None;
                for d in devices {
                    let n = d.name().unwrap_or_default();
                    if n == name || n.contains(name) {
                        found = Some(d);
                        break;
                    }
                }

                found.ok_or_else(|| {
                    AppError::NotFound(format!("Recording device '{}' not found", name))
                })?
            }
            None => host.default_input_device().ok_or_else(|| {
                AppError::NotFound("No default recording device found".to_string())
            })?,
        };

        let name = device.name().unwrap_or_else(|_| "unknown".to_string());
        info!(device = %name, "Opening microphone capture device");

        let default_config = device.default_input_config().map_err(|e| {
            AppError::Command(format!("Failed to get default input config: {e}"))
        })?;

        let channels = default_config.channels().min(1);
        let config = StreamConfig {
            channels,
            sample_rate: cpal::SampleRate(48_000),
            buffer_size: cpal::BufferSize::Fixed(480),
        };

        let stream = device
            .build_input_stream(
                &config.into(),
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    // If we got stereo from the device, downmix to mono
                    if channels > 1 {
                        let mono: Vec<f32> = data
                            .chunks(channels as usize)
                            .map(|chunk| chunk.iter().sum::<f32>() / channels as f32)
                            .collect();
                        let chunk = CaptureChunk {
                            timestamp: std::time::Instant::now(),
                            samples: mono,
                        };
                        let _ = tx.try_send(chunk);
                    } else {
                        let chunk = CaptureChunk {
                            timestamp: std::time::Instant::now(),
                            samples: data.to_vec(),
                        };
                        let _ = tx.try_send(chunk);
                    }
                },
                |err| {
                    warn!("Mic capture stream error: {err}");
                },
                None,
            )
            .map_err(|e| AppError::Command(format!("Failed to build input stream: {e}")))?;

        stream.play().map_err(|e| {
            AppError::Command(format!("Failed to start mic stream: {e}"))
        })?;

        info!(device = %name, "Microphone capture started (48kHz mono f32, 10ms chunks)");

        Ok(MicCaptureDevice { stream, name })
    }

    /// Return the device name.
    pub fn name(&self) -> Option<&str> {
        Some(&self.name)
    }

    /// Pause capture.
    pub fn pause(&self) {
        if let Err(e) = self.stream.pause() {
            warn!("Failed to pause mic stream: {e}");
        }
    }

    /// Resume after pause.
    pub fn resume(&self) {
        if let Err(e) = self.stream.play() {
            warn!("Failed to resume mic stream: {e}");
        }
    }
}

impl Drop for MicCaptureDevice {
    fn drop(&mut self) {
        // Stream stops automatically on drop
    }
}
