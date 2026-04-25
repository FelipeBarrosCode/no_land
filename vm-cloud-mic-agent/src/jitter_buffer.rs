use std::collections::BinaryHeap;
use std::time::{Duration, Instant};

use tracing::warn;

/// A simple adaptive jitter buffer for RTP packets.
///
/// MVP: orders packets by sequence, drops late packets,
/// releases after target delay. No PLC yet.
#[derive(Debug, Clone)]
pub struct JitterBuffer {
    target_delay_ms: u32,
    max_delay_ms: u32,
    packets: BinaryHeap<BufferedPacket>,
    last_released_seq: Option<u16>,
    late_drops: u64,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct BufferedPacket {
    seq: u16,
    received_at: Instant,
    payload: Vec<u8>,
}

impl Ord for BufferedPacket {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Reverse ordering so BinaryHeap pops smallest seq first
        other.seq.cmp(&self.seq)
    }
}

impl PartialOrd for BufferedPacket {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl JitterBuffer {
    pub fn new(target_delay_ms: u32, max_delay_ms: u32) -> Self {
        Self {
            target_delay_ms,
            max_delay_ms,
            packets: BinaryHeap::new(),
            last_released_seq: None,
            late_drops: 0,
        }
    }

    pub fn push(&mut self, seq: u16, payload: Vec<u8>) {
        let now = Instant::now();

        // Check if packet is too late
        if let Some(last_seq) = self.last_released_seq {
            let gap = seq.wrapping_sub(last_seq);
            if gap > 100 && seq < last_seq {
                // Likely a sequence wrap or very old packet
                self.late_drops += 1;
                return;
            }
        }

        self.packets.push(BufferedPacket {
            seq,
            received_at: now,
            payload,
        });
    }

    /// Try to pop the next packet if target delay has elapsed.
    pub fn try_pop(&mut self) -> Option<Vec<u8>> {
        let now = Instant::now();
        let front = self.packets.peek()?;

        let elapsed = now.duration_since(front.received_at).as_millis() as u32;
        if elapsed < self.target_delay_ms {
            return None;
        }

        let packet = self.packets.pop()?;
        self.last_released_seq = Some(packet.seq);
        Some(packet.payload)
    }

    /// Pop all packets that are past max delay (emergency drain).
    pub fn drain_overdue(&mut self) -> Vec<Vec<u8>> {
        let now = Instant::now();
        let mut result = Vec::new();

        while let Some(front) = self.packets.peek() {
            let elapsed = now.duration_since(front.received_at).as_millis() as u32;
            if elapsed > self.max_delay_ms {
                let packet = self.packets.pop().unwrap();
                self.last_released_seq = Some(packet.seq);
                result.push(packet.payload);
            } else {
                break;
            }
        }

        result
    }

    pub fn depth_ms(&self) -> u32 {
        if let Some(front) = self.packets.peek() {
            Instant::now().duration_since(front.received_at).as_millis() as u32
        } else {
            0
        }
    }

    pub fn len(&self) -> usize {
        self.packets.len()
    }

    pub fn late_drops(&self) -> u64 {
        self.late_drops
    }

    pub fn clear(&mut self) {
        self.packets.clear();
        self.last_released_seq = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn test_ordering() {
        let mut buf = JitterBuffer::new(10, 100);
        buf.push(3, vec![3]);
        buf.push(1, vec![1]);
        buf.push(2, vec![2]);

        sleep(Duration::from_millis(20));

        assert_eq!(buf.try_pop(), Some(vec![1]));
        assert_eq!(buf.try_pop(), Some(vec![2]));
        assert_eq!(buf.try_pop(), Some(vec![3]));
    }

    #[test]
    fn test_not_ready_yet() {
        let mut buf = JitterBuffer::new(50, 100);
        buf.push(1, vec![1]);
        assert_eq!(buf.try_pop(), None);
    }

    #[test]
    fn test_late_packet_drop() {
        let mut buf = JitterBuffer::new(10, 100);
        buf.push(1, vec![1]);
        sleep(Duration::from_millis(20));
        assert_eq!(buf.try_pop(), Some(vec![1]));

        // Very old packet should be dropped
        buf.push(0, vec![0]);
        assert_eq!(buf.late_drops(), 1);
    }
}
