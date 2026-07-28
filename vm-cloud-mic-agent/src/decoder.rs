use audiopus::coder::Decoder as OpusDecoderInner;
use audiopus::packet::Packet;
use audiopus::{Channels, MutSignals, SampleRate};
use std::convert::TryFrom;
use tracing::{error, warn};

const SAMPLE_RATE: SampleRate = SampleRate::Hz48000;
const CHANNELS: Channels = Channels::Mono;
const FRAME_SIZE: usize = 480; // samples per 10ms at 48kHz

/// Wraps an Opus decoder for microphone audio.
pub struct OpusMicDecoder {
    decoder: OpusDecoderInner,
    /// Internal buffer for decoded PCM (reused across frames).
    pcm_buf: Vec<f32>,
    /// Whether PLC is active (previous frame was lost).
    plc_active: bool,
}

impl OpusMicDecoder {
    /// Create a new decoder.
    pub fn new() -> Result<Self, String> {
        let decoder = OpusDecoderInner::new(SAMPLE_RATE, CHANNELS)
            .map_err(|e| format!("Opus decoder creation failed: {e}"))?;

        Ok(Self {
            decoder,
            pcm_buf: vec![0.0f32; FRAME_SIZE],
            plc_active: false,
        })
    }

    /// Decode an Opus frame to float32 PCM.
    ///
    /// Returns a slice of 480 float32 samples. The slice is valid until the
    /// next call to `decode` or `decode_plc`.
    pub fn decode(&mut self, opus_data: &[u8]) -> Result<&[f32], String> {
        self.plc_active = false;

        let packet =
            Packet::try_from(opus_data).map_err(|e| format!("Invalid Opus packet: {e}"))?;
        let signals = MutSignals::try_from(&mut self.pcm_buf[..])
            .map_err(|e| format!("Invalid signal buffer: {e}"))?;

        let len = self
            .decoder
            .decode_float(Some(packet), signals, false)
            .map_err(|e| format!("Opus decode failed: {e}"))?;

        if len != FRAME_SIZE {
            return Err(format!(
                "Opus decode produced {len} samples, expected {FRAME_SIZE}"
            ));
        }

        Ok(&self.pcm_buf[..FRAME_SIZE])
    }

    /// Decode a missing frame using Opus Packet Loss Concealment (PLC).
    pub fn decode_plc(&mut self) -> &[f32] {
        self.plc_active = true;

        let signals = match MutSignals::try_from(&mut self.pcm_buf[..]) {
            Ok(s) => s,
            Err(e) => {
                error!("PLC signal buffer error: {e}");
                self.pcm_buf.fill(0.0);
                return &self.pcm_buf[..FRAME_SIZE];
            }
        };

        match self.decoder.decode_float(None, signals, true) {
            Ok(len) if len == FRAME_SIZE => &self.pcm_buf[..FRAME_SIZE],
            Ok(len) => {
                warn!("PLC produced {len} samples, expected {FRAME_SIZE}");
                if len < FRAME_SIZE {
                    self.pcm_buf[len..FRAME_SIZE].fill(0.0);
                }
                &self.pcm_buf[..FRAME_SIZE]
            }
            Err(e) => {
                error!("Opus PLC failed: {e}");
                self.pcm_buf.fill(0.0);
                &self.pcm_buf[..FRAME_SIZE]
            }
        }
    }

    /// Reset the decoder state (after a discontinuity, etc.).
    pub fn reset(&mut self) {
        self.plc_active = false;
        if let Ok(new_dec) = OpusDecoderInner::new(SAMPLE_RATE, CHANNELS) {
            self.decoder = new_dec;
        }
    }
}

/// Convert float32 PCM to raw little-endian i16 bytes.
pub fn float32_to_i16_bytes(samples: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        let clamped = sample.clamp(-1.0, 1.0);
        let i16_val = (clamped * 32767.0) as i16;
        bytes.extend_from_slice(&i16_val.to_le_bytes());
    }
    bytes
}
