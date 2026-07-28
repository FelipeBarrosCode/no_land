use std::collections::VecDeque;
use std::io::Write;
use std::time::Instant;

use noland_mic_protocol::packet::ParsedPacket;
use tracing::{debug, info, warn};

use crate::auth_session::AuthSession;
use crate::config::{AudioConfig, JitterConfig};
use crate::decoder::{float32_to_i16_bytes, OpusMicDecoder};
use crate::jitter::JitterBuffer;

/// Orchestrates the receive pipeline: UDP → auth → jitter → decode → PCM output.
pub struct Receiver {
    config: AudioConfig,
    decoder: OpusMicDecoder,
    jitter: JitterBuffer,
    /// Active session (if any).
    session: Option<AuthSession>,
    /// Decoded PCM output queue (raw i16 bytes, ready for PipeWire).
    pcm_queue: VecDeque<u8>,
    /// Time of last valid packet.
    last_packet_time: Option<Instant>,
    /// Whether we're currently outputting silence.
    silent: bool,
    /// Stats reporting interval.
    stats_timer: Instant,
    /// Scratch buffer for writing chunks to output.
    scratch: Vec<u8>,
    /// Number of consecutively produced frames this tick.
    tick_frame_count: u32,
}

/// Maximum frames to decode per tick to keep the real-time loop responsive.
const MAX_FRAMES_PER_TICK: u32 = 5;

impl Receiver {
    pub fn new(audio_config: AudioConfig, jitter_config: JitterConfig) -> Self {
        let decoder = OpusMicDecoder::new().expect("Opus decoder creation should succeed");

        let jitter = JitterBuffer::new(
            jitter_config.initial_ms,
            jitter_config.minimum_ms,
            jitter_config.maximum_ms,
            jitter_config.reorder_window_packets,
            audio_config.sample_rate,
            audio_config.frame_duration_ms,
        );

        Self {
            config: audio_config,
            decoder,
            jitter,
            session: None,
            pcm_queue: VecDeque::with_capacity(64 * 960),
            last_packet_time: None,
            silent: true,
            stats_timer: Instant::now(),
            scratch: Vec::with_capacity(4096),
            tick_frame_count: 0,
        }
    }

    /// Process a raw UDP packet.
    pub fn process_packet(&mut self, buf: &[u8]) {
        let packet = match ParsedPacket::parse(buf) {
            Ok(p) => p,
            Err(e) => {
                warn!("Failed to parse mic packet: {e}");
                return;
            }
        };

        // For MVP: auto-activate on first packet (no control socket yet)
        if self.session.is_none() {
            info!(
                session_id = packet.session_id,
                ssrc = packet.ssrc,
                "Auto-activating session"
            );
            self.session = Some(AuthSession::new(
                packet.session_id,
                packet.ssrc,
                b"",
                Instant::now() + std::time::Duration::from_secs(3600),
            ));
        }

        if let Some(ref mut session) = self.session {
            if packet.session_id != session.session_id {
                info!(
                    old = session.session_id,
                    new = packet.session_id,
                    "Session changed"
                );
                self.session = Some(AuthSession::new(
                    packet.session_id,
                    packet.ssrc,
                    b"",
                    Instant::now() + std::time::Duration::from_secs(3600),
                ));
            }
        }

        self.last_packet_time = Some(Instant::now());
        self.silent = false;

        // End-of-stream
        if packet.is_eos() {
            info!("End-of-stream received");
            if let Some(ref mut s) = self.session {
                s.deactivate();
            }
            self.session = None;
            return;
        }

        // Discontinuity → reset decoder
        if packet.is_discontinuity() {
            info!("Discontinuity — resetting decoder");
            self.decoder.reset();
            self.jitter = JitterBuffer::new(
                20.0,
                10.0,
                40.0,
                64,
                self.config.sample_rate,
                self.config.frame_duration_ms,
            );
        }

        // Insert into jitter buffer (even muted/silence markers)
        let opus_data = if packet.is_muted() {
            vec![0xFC, 0xFF, 0xFE] // minimal Opus silence
        } else {
            packet.opus_payload.to_vec()
        };

        self.jitter.insert(
            packet.sequence,
            packet.timestamp,
            packet.has_fec(),
            opus_data,
        );
    }

    /// Called periodically to drive the jitter buffer and decode ready frames.
    pub fn tick(&mut self, _output: &mut impl Write) {
        self.tick_frame_count = 0;

        // Session timeout check
        if let Some(ref session) = self.session {
            if !session.is_active() {
                self.session = None;
                self.silent = true;
            }
        }

        // Stall detection
        if let Some(last) = self.last_packet_time {
            if last.elapsed().as_millis() > 500 && !self.silent {
                self.silent = true;
            }
        }

        // Pop and decode frames
        while self.tick_frame_count < MAX_FRAMES_PER_TICK {
            if let Some(frame) = self.jitter.pop() {
                self.decode_and_enqueue(frame.opus_data);
            } else if self.jitter.consecutive_misses() > 0 {
                // Lost frame — use PLC
                let samples = self.decoder.decode_plc();
                let bytes = float32_to_i16_bytes(samples);
                self.pcm_queue.extend(bytes);
                self.tick_frame_count += 1;
            } else {
                // Buffer is idle (playhead caught up to network)
                break;
            }
        }

        // Periodic stats
        if self.stats_timer.elapsed().as_secs() >= 10 {
            let stats = self.jitter.drain_stats();
            debug!(
                jitter_ms = %stats.smoothed_jitter_ms,
                target_ms = %stats.target_delay_ms,
                depth_ms = %stats.buffer_depth_ms,
                received = stats.packets_received,
                lost = stats.packets_lost,
                "Receiver stats"
            );
            self.stats_timer = Instant::now();
        }
    }

    fn decode_and_enqueue(&mut self, opus_data: Vec<u8>) {
        match self.decoder.decode(&opus_data) {
            Ok(samples) => {
                let bytes = float32_to_i16_bytes(samples);
                self.pcm_queue.extend(bytes);
            }
            Err(e) => {
                warn!("Decode error: {e}");
                let samples = self.decoder.decode_plc();
                let bytes = float32_to_i16_bytes(samples);
                self.pcm_queue.extend(bytes);
            }
        }
        self.tick_frame_count += 1;
    }

    /// Drain decoded PCM from the queue. Writes at most ~20ms of audio per
    /// call to keep latency bounded. Writes silence when idle.
    pub fn drain_pcm(&mut self, output: &mut impl Write) {
        // How many bytes we aim to write (20ms * 48000Hz * 2 bytes = 1920)
        let target_bytes = 1920usize;

        if self.silent {
            let silence = vec![0u8; target_bytes];
            let _ = output.write_all(&silence);
            return;
        }

        let available = self.pcm_queue.len().min(target_bytes);
        if available == 0 {
            // No data — output a small silence chunk to keep pipe fed
            let silence = vec![0u8; 960]; // 10ms
            let _ = output.write_all(&silence);
            return;
        }

        // Drain in efficient chunks
        self.scratch.clear();
        self.scratch.reserve(available);
        for _ in 0..available {
            if let Some(b) = self.pcm_queue.pop_front() {
                self.scratch.push(b);
            } else {
                break;
            }
        }
        let _ = output.write_all(&self.scratch);

        // If we wrote less than target, pad with silence
        if self.scratch.len() < target_bytes {
            let padding = target_bytes - self.scratch.len();
            let silence = vec![0u8; padding];
            let _ = output.write_all(&silence);
        }
    }

    /// Flush all remaining PCM and reset state.
    pub fn flush(&mut self, output: &mut impl Write) {
        while !self.pcm_queue.is_empty() {
            self.scratch.clear();
            let chunk = self.pcm_queue.len().min(4096);
            for _ in 0..chunk {
                if let Some(b) = self.pcm_queue.pop_front() {
                    self.scratch.push(b);
                }
            }
            let _ = output.write_all(&self.scratch);
        }
        output.flush().ok();
    }
}
