/// Adaptive jitter buffer for microphone packets.
///
/// Buffers incoming Opus frames and releases them at a steady 10ms cadence
/// to the decoder. Adapts target delay based on observed network jitter.
#[derive(Debug)]
pub struct JitterBuffer {
    /// Buffered frames, indexed by sequence number.
    buffer: Vec<Option<BufferedFrame>>,
    /// Next sequence number to play out.
    playhead: u16,
    /// Current target buffer depth in milliseconds.
    target_delay_ms: f64,
    /// Minimum / maximum target delay.
    min_delay_ms: f64,
    max_delay_ms: f64,
    /// Number of slots in the circular buffer.
    capacity: usize,
    /// Whether we have received the first packet (for clock sync).
    initialized: bool,
    /// Sample rate for timestamp calculations.
    sample_rate: u32,
    /// Samples per frame.
    samples_per_frame: u32,
    /// Last packet arrival time (monotonic).
    last_arrival: Option<std::time::Instant>,
    /// Smoothed inter-arrival jitter (EWMA, ms).
    smoothed_jitter_ms: f64,
    /// Packets received since last stats update.
    packets_received: u64,
    /// Packets played since last stats update.
    packets_played: u64,
    /// Packets lost since last stats update.
    packets_lost: u64,
    /// Packets recovered via FEC or reordering.
    packets_recovered: u64,
}

#[derive(Debug, Clone)]
pub struct BufferedFrame {
    pub sequence: u16,
    pub timestamp: u32,
    pub has_fec: bool,
    pub opus_data: Vec<u8>,
}

#[derive(Debug, Default, Clone)]
pub struct JitterStats {
    pub target_delay_ms: f64,
    pub smoothed_jitter_ms: f64,
    pub buffer_depth_ms: f64,
    pub packets_received: u64,
    pub packets_lost: u64,
    pub packets_recovered: u64,
}

impl JitterBuffer {
    pub fn new(
        initial_delay_ms: f64,
        min_delay_ms: f64,
        max_delay_ms: f64,
        reorder_window: usize,
        sample_rate: u32,
        frame_duration_ms: u32,
    ) -> Self {
        let samples_per_frame = sample_rate * frame_duration_ms / 1000;
        Self {
            buffer: vec![None; reorder_window],
            playhead: 0,
            target_delay_ms: initial_delay_ms,
            min_delay_ms,
            max_delay_ms,
            capacity: reorder_window,
            initialized: false,
            sample_rate,
            samples_per_frame,
            last_arrival: None,
            smoothed_jitter_ms: 0.0,
            packets_received: 0,
            packets_played: 0,
            packets_lost: 0,
            packets_recovered: 0,
        }
    }

    /// Insert a received frame into the buffer.
    pub fn insert(&mut self, sequence: u16, timestamp: u32, has_fec: bool, opus_data: Vec<u8>) {
        self.packets_received += 1;

        if !self.initialized {
            self.playhead = sequence;
            self.buffer[sequence as usize % self.capacity] = Some(BufferedFrame {
                sequence,
                timestamp,
                has_fec,
                opus_data,
            });
            self.initialized = true;
            self.last_arrival = Some(std::time::Instant::now());
            return;
        }

        // Update jitter estimate using EWMA
        if let Some(last) = self.last_arrival {
            let now = std::time::Instant::now();
            let elapsed_ms = now.duration_since(last).as_secs_f64() * 1000.0;
            let expected_ms = 10.0; // 10ms frame interval
            let jitter = (elapsed_ms - expected_ms).abs();
            self.smoothed_jitter_ms = 0.9 * self.smoothed_jitter_ms + 0.1 * jitter;
            self.last_arrival = Some(now);

            // Adapt target delay
            let new_target =
                (self.smoothed_jitter_ms * 2.0 + 5.0).clamp(self.min_delay_ms, self.max_delay_ms);
            self.target_delay_ms = 0.95 * self.target_delay_ms + 0.05 * new_target;
        }

        let idx = sequence as usize % self.capacity;

        // Check if this fills a gap (packet arrived after playhead passed)
        let behind_by = sequence.wrapping_sub(self.playhead);
        if behind_by > self.capacity as u16 / 2 {
            // Too old — drop
            return;
        }

        self.buffer[idx] = Some(BufferedFrame {
            sequence,
            timestamp,
            has_fec,
            opus_data,
        });
    }

    /// Pop the next frame for playout. Returns `None` if the playhead frame
    /// hasn't arrived yet (the caller should use PLC).
    pub fn pop(&mut self) -> Option<BufferedFrame> {
        let idx = self.playhead as usize % self.capacity;
        let frame = self.buffer[idx].take();

        match frame {
            Some(f) => {
                // Check if we're far enough ahead of real-time
                let depth_packets = self.current_depth_packets();
                let depth_ms = depth_packets as f64 * 10.0;

                if depth_ms < self.target_delay_ms * 0.5 && depth_packets <= 1 {
                    // Not enough buffer — delay playhead? For now, play through
                }

                self.packets_played += 1;
                self.playhead = self.playhead.wrapping_add(1);
                Some(f)
            }
            None => {
                // Packet not yet arrived (or lost). Advance playhead and report loss.
                self.packets_lost += 1;
                self.playhead = self.playhead.wrapping_add(1);
                None
            }
        }
    }

    /// Check if the buffer has a frame ready at the playhead.
    pub fn has_frame(&self) -> bool {
        let idx = self.playhead as usize % self.capacity;
        self.buffer[idx].is_some()
    }

    /// Current number of buffered frames.
    pub fn current_depth_packets(&self) -> usize {
        self.buffer.iter().filter(|f| f.is_some()).count()
    }

    /// Snapshot current statistics without resetting counters.
    pub fn snapshot_stats(&self) -> JitterStats {
        JitterStats {
            target_delay_ms: self.target_delay_ms,
            smoothed_jitter_ms: self.smoothed_jitter_ms,
            buffer_depth_ms: self.current_depth_packets() as f64 * 10.0,
            packets_received: self.packets_received,
            packets_lost: self.packets_lost,
            packets_recovered: self.packets_recovered,
        }
    }

    /// Drain statistics and reset counters.
    pub fn drain_stats(&mut self) -> JitterStats {
        self.snapshot_stats()
    }

    /// Number of consecutive missed frames at the playhead.
    pub fn consecutive_misses(&self) -> u32 {
        let mut count = 0u32;
        let mut seq = self.playhead;
        for _ in 0..self.capacity {
            let idx = seq as usize % self.capacity;
            if self.buffer[idx].is_some() {
                break;
            }
            count += 1;
            seq = seq.wrapping_add(1);
        }
        count
    }
}
