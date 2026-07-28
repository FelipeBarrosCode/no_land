use audiopus::coder::Encoder as OpusEncoderInner;
use audiopus::{Application, Bitrate, Channels, SampleRate, Signal};
use std::convert::TryFrom;
use tracing::warn;

use crate::errors::{AppError, AppResult};

const SAMPLE_RATE: SampleRate = SampleRate::Hz48000;
const CHANNELS: Channels = Channels::Mono;
const FRAME_SIZE: usize = 480;

/// Wraps an Opus encoder configured for voice communication.
pub struct OpusMicEncoder {
    encoder: OpusEncoderInner,
}

impl OpusMicEncoder {
    /// Create a new encoder with the given bitrate in bps (e.g., 48000).
    pub fn new(bitrate_bps: i32) -> AppResult<Self> {
        let mut encoder = OpusEncoderInner::new(SAMPLE_RATE, CHANNELS, Application::Voip)
            .map_err(|e| AppError::Command(format!("Opus encoder creation failed: {e}")))?;

        let bitrate = Bitrate::try_from(bitrate_bps).unwrap_or(Bitrate::BitsPerSecond(48000));
        encoder
            .set_bitrate(bitrate)
            .map_err(|e| AppError::Command(format!("Opus set_bitrate failed: {e}")))?;
        encoder
            .set_vbr(true)
            .map_err(|e| AppError::Command(format!("Opus set_vbr failed: {e}")))?;
        encoder
            .set_signal(Signal::Voice)
            .map_err(|e| AppError::Command(format!("Opus set_signal failed: {e}")))?;
        encoder
            .set_complexity(6)
            .map_err(|e| AppError::Command(format!("Opus set_complexity failed: {e}")))?;
        encoder
            .set_inband_fec(true)
            .map_err(|e| AppError::Command(format!("Opus set_inband_fec failed: {e}")))?;

        Ok(Self { encoder })
    }

    /// Encode a float32 mono PCM frame into a fresh `Vec<u8>` of Opus data.
    pub fn encode(&self, samples: &[f32]) -> AppResult<Vec<u8>> {
        let mut buf = vec![0u8; 256];
        let len = self
            .encoder
            .encode_float(&samples[..FRAME_SIZE], &mut buf)
            .map_err(|e| AppError::Command(format!("Opus encode failed: {e}")))?;
        buf.truncate(len);
        Ok(buf)
    }

    /// Encode a silence frame.
    pub fn encode_silence(&self) -> Vec<u8> {
        let silence = vec![0.0f32; FRAME_SIZE];
        match self.encode(&silence) {
            Ok(buf) => buf,
            Err(e) => {
                warn!("Opus silence encode failed: {e}");
                vec![0xFC, 0xFF, 0xFE]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_encoder() {
        let enc = OpusMicEncoder::new(48000);
        assert!(enc.is_ok(), "Should create encoder at 48kbps");
    }

    #[test]
    fn test_encode_silence() {
        let enc = OpusMicEncoder::new(48000).expect("encoder");
        let out = enc.encode_silence();
        assert!(!out.is_empty(), "Silence should produce output");
    }

    #[test]
    fn test_encode_valid_frame() {
        let enc = OpusMicEncoder::new(48000).expect("encoder");
        let samples: Vec<f32> = (0..480).map(|i| (i as f32 * 0.001).sin() * 0.1).collect();
        let out = enc.encode(&samples).expect("encode");
        assert!(!out.is_empty());
    }
}
