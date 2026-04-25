use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use serde::Serialize;

use crate::rtp::RtpPacket;

/// Metrics collector for the mic agent.
pub struct MetricsCollector {
    packets_received_total: AtomicU64,
    packets_lost_total: AtomicU64,
    late_packets_total: AtomicU64,
    decode_errors_total: AtomicU64,
    last_packet_time: std::sync::Mutex<Option<Instant>>,
    last_seq: std::sync::Mutex<Option<u16>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MetricsSnapshot {
    pub packets_received_total: u64,
    pub packets_received_1s: u64,
    pub packets_lost_total: u64,
    pub late_packets_total: u64,
    pub decode_errors_total: u64,
    pub jitter_ms: f64,
    pub buffer_depth_ms: f64,
    pub last_packet_ms_ago: Option<u64>,
}

impl MetricsCollector {
    pub fn new() -> Self {
        Self {
            packets_received_total: AtomicU64::new(0),
            packets_lost_total: AtomicU64::new(0),
            late_packets_total: AtomicU64::new(0),
            decode_errors_total: AtomicU64::new(0),
            last_packet_time: std::sync::Mutex::new(None),
            last_seq: std::sync::Mutex::new(None),
        }
    }

    pub fn record_packet(&self, packet: &RtpPacket) {
        self.packets_received_total.fetch_add(1, Ordering::Relaxed);

        let mut last_seq = self.last_seq.lock().unwrap();
        if let Some(prev) = *last_seq {
            let expected = prev.wrapping_add(1);
            if packet.sequence_number != expected {
                let lost = packet.sequence_number.wrapping_sub(expected);
                if lost < 100 {
                    self.packets_lost_total
                        .fetch_add(lost as u64, Ordering::Relaxed);
                }
            }
        }
        *last_seq = Some(packet.sequence_number);
        drop(last_seq);

        let mut last_time = self.last_packet_time.lock().unwrap();
        *last_time = Some(Instant::now());
    }

    pub fn record_decode_error(&self) {
        self.decode_errors_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        let now = Instant::now();
        let last_time = self.last_packet_time.lock().unwrap();
        let last_packet_ms_ago = last_time.map(|t| now.duration_since(t).as_millis() as u64);

        MetricsSnapshot {
            packets_received_total: self.packets_received_total.load(Ordering::Relaxed),
            packets_received_1s: 0, // TODO: track 1s window
            packets_lost_total: self.packets_lost_total.load(Ordering::Relaxed),
            late_packets_total: self.late_packets_total.load(Ordering::Relaxed),
            decode_errors_total: self.decode_errors_total.load(Ordering::Relaxed),
            jitter_ms: 0.0,       // TODO
            buffer_depth_ms: 0.0, // TODO
            last_packet_ms_ago,
        }
    }

    pub fn packet_loss_percent(&self) -> f64 {
        let received = self.packets_received_total.load(Ordering::Relaxed) as f64;
        let lost = self.packets_lost_total.load(Ordering::Relaxed) as f64;
        let total = received + lost;
        if total > 0.0 {
            (lost / total) * 100.0
        } else {
            0.0
        }
    }

    pub fn jitter_ms(&self) -> f64 {
        0.0 // TODO
    }

    pub fn buffer_depth_ms(&self) -> f64 {
        0.0 // TODO
    }

    pub fn last_packet_ms_ago(&self) -> Option<u64> {
        let last_time = self.last_packet_time.lock().unwrap();
        last_time.map(|t| Instant::now().duration_since(t).as_millis() as u64)
    }
}
