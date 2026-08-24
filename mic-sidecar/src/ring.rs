use std::sync::atomic::{AtomicI16, AtomicU64, Ordering};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PushReport {
    pub written: usize,
    pub dropped_stale: u64,
    pub overrun: bool,
}

struct Slot {
    value: AtomicI16,
    sequence: AtomicU64,
}

/// A bounded, lock-free single-producer/single-consumer audio ring.
///
/// The producer advances `read` when full, so the oldest (stale) audio is
/// discarded instead of increasing end-to-end latency. Atomic slots keep an
/// overwrite racing a consumer memory-safe; sequence validation turns a raced
/// sample into silence rather than exposing torn/stale audio.
pub struct AudioRing {
    slots: Box<[Slot]>,
    capacity: u64,
    write: AtomicU64,
    read: AtomicU64,
}

impl AudioRing {
    pub fn new(capacity_samples: usize) -> Self {
        assert!(capacity_samples > 0, "audio ring capacity must be non-zero");
        let slots = (0..capacity_samples)
            .map(|_| Slot {
                value: AtomicI16::new(0),
                sequence: AtomicU64::new(0),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            slots,
            capacity: capacity_samples as u64,
            write: AtomicU64::new(0),
            read: AtomicU64::new(0),
        }
    }

    pub fn len(&self) -> usize {
        let write = self.write.load(Ordering::Acquire);
        let read = self.read.load(Ordering::Acquire);
        write.saturating_sub(read).min(self.capacity) as usize
    }

    pub fn clear(&self) {
        let write = self.write.load(Ordering::Acquire);
        self.read.store(write, Ordering::Release);
    }

    /// Pushes samples and drops enough old samples to retain only the newest
    /// `capacity()` samples. This method must have exactly one producer.
    pub fn push_slice(&self, samples: &[i16]) -> PushReport {
        self.push_from_iter(samples.len(), samples.iter().copied())
    }

    /// Converts/copies an exact-size iterator directly into the ring without
    /// an intermediate allocation. Useful from real-time capture callbacks.
    pub fn push_from_iter<I>(&self, sample_count: usize, samples: I) -> PushReport
    where
        I: IntoIterator<Item = i16>,
    {
        if sample_count == 0 {
            return PushReport::default();
        }

        let write = self.write.load(Ordering::Relaxed);
        let total = sample_count as u64;
        let new_write = write.saturating_add(total);
        let minimum_read = new_write.saturating_sub(self.capacity);
        let mut dropped = 0;

        loop {
            let read = self.read.load(Ordering::Acquire);
            if read >= minimum_read {
                break;
            }
            match self.read.compare_exchange_weak(
                read,
                minimum_read,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    dropped = minimum_read - read;
                    break;
                }
                Err(_) => continue,
            }
        }

        let skip = sample_count.saturating_sub(self.capacity as usize);
        for (offset, sample) in samples.into_iter().enumerate().skip(skip) {
            let position = write + offset as u64;
            let slot = &self.slots[(position % self.capacity) as usize];
            slot.sequence.store(0, Ordering::Release);
            slot.value.store(sample, Ordering::Relaxed);
            slot.sequence.store(position + 1, Ordering::Release);
        }
        self.write.store(new_write, Ordering::Release);

        PushReport {
            written: sample_count - skip,
            dropped_stale: dropped,
            overrun: dropped > 0,
        }
    }

    /// Pops up to `output.len()` samples. This method must have exactly one
    /// consumer. Sequence-invalid samples are replaced by silence.
    pub fn pop_slice(&self, output: &mut [i16]) -> usize {
        if output.is_empty() {
            return 0;
        }

        loop {
            let read = self.read.load(Ordering::Acquire);
            let write = self.write.load(Ordering::Acquire);
            let count = output
                .len()
                .min(write.saturating_sub(read).min(self.capacity) as usize);
            if count == 0 {
                return 0;
            }
            if self
                .read
                .compare_exchange_weak(
                    read,
                    read + count as u64,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_err()
            {
                continue;
            }

            for (offset, destination) in output.iter_mut().take(count).enumerate() {
                let position = read + offset as u64;
                let slot = &self.slots[(position % self.capacity) as usize];
                let expected = position + 1;
                let before = slot.sequence.load(Ordering::Acquire);
                let value = slot.value.load(Ordering::Relaxed);
                let after = slot.sequence.load(Ordering::Acquire);
                *destination = if before == expected && after == expected {
                    value
                } else {
                    0
                };
            }
            return count;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overflow_discards_stale_audio() {
        let ring = AudioRing::new(4);
        ring.push_slice(&[1, 2, 3]);
        let report = ring.push_slice(&[4, 5, 6]);
        assert_eq!(report.dropped_stale, 2);
        assert!(report.overrun);

        let mut output = [0; 4];
        assert_eq!(ring.pop_slice(&mut output), 4);
        assert_eq!(output, [3, 4, 5, 6]);
    }

    #[test]
    fn oversized_write_keeps_only_newest_capacity() {
        let ring = AudioRing::new(3);
        let report = ring.push_slice(&[10, 11, 12, 13, 14]);
        assert_eq!(report.written, 3);
        assert_eq!(report.dropped_stale, 2);
        let mut output = [0; 3];
        assert_eq!(ring.pop_slice(&mut output), 3);
        assert_eq!(output, [12, 13, 14]);
    }
}
