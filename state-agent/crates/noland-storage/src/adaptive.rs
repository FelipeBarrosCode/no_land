use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use noland_rclone_adapter::{RemoteErrorClass, TransferTuning};
use noland_state_core::Result;
use tokio::sync::Notify;

/// Raises and lowers in-flight transfer slots from byte-normalized successful
/// throughput and remote backpressure. Pack size alone is not a congestion signal.
///
/// Weighted reservations account for all transfers inside a bulk rclone process,
/// as well as individual copyto processes, against the same budget.
pub struct AdaptiveConcurrency {
    min: usize,
    max: usize,
    current: AtomicUsize,
    active: AtomicUsize,
    backpressured: AtomicBool,
    feedback: std::sync::Mutex<Feedback>,
    notify: Notify,
}

#[derive(Default)]
struct Feedback {
    bytes_per_second: f64,
    healthy: u32,
    slow: u32,
}

pub struct AdaptivePermit<'a> {
    limiter: &'a AdaptiveConcurrency,
    slots: usize,
}

impl AdaptivePermit<'_> {
    pub fn slots(&self) -> usize {
        self.slots
    }

    pub(crate) fn retain_slots(&mut self, remaining: usize) {
        // Retain a process slot until exit; only completed objects release capacity.
        let next = self.slots.min(remaining.max(1));
        self.limiter
            .active
            .fetch_sub(self.slots - next, Ordering::SeqCst);
        self.slots = next;
        self.limiter.notify.notify_waiters();
    }
}

impl AdaptiveConcurrency {
    pub fn from_tuning(tuning: &TransferTuning) -> Arc<Self> {
        let max = tuning
            .max_parallel_uploads
            .max(tuning.max_parallel_downloads)
            .max(1);
        Self::new(tuning.max_parallel_uploads.max(1).min(max), 1, max)
    }

    pub fn for_uploads(tuning: &TransferTuning) -> Arc<Self> {
        let max = tuning.max_parallel_uploads.max(1);
        Self::new(max, 1, max)
    }

    pub fn for_downloads(tuning: &TransferTuning) -> Arc<Self> {
        let max = tuning.parallel_download_processes().max(1);
        Self::new(max, 1, max)
    }

    /// `rclone cat` has one ordered connection per process, unlike copyto.
    pub fn for_streams(tuning: &TransferTuning) -> Arc<Self> {
        let max = tuning.max_parallel_downloads.clamp(1, 32);
        Self::new(max, 1, max)
    }

    pub fn new(initial: usize, min: usize, max: usize) -> Arc<Self> {
        let max = max.max(1);
        let min = min.max(1).min(max);
        let initial = initial.clamp(min, max);
        Arc::new(Self {
            min,
            max,
            current: AtomicUsize::new(initial),
            active: AtomicUsize::new(0),
            backpressured: AtomicBool::new(false),
            feedback: std::sync::Mutex::new(Feedback::default()),
            notify: Notify::new(),
        })
    }

    pub fn current(&self) -> usize {
        self.current.load(Ordering::SeqCst)
    }

    pub fn min(&self) -> usize {
        self.min
    }

    pub fn max(&self) -> usize {
        self.max
    }

    pub async fn acquire(&self) -> Result<AdaptivePermit<'_>> {
        self.acquire_slots(1).await
    }

    pub async fn acquire_slots(&self, requested: usize) -> Result<AdaptivePermit<'_>> {
        loop {
            // Register before checking capacity, so a release between the check
            // and await cannot be lost. No capacity is held across an await.
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let current = self.current();
            let slots = requested.max(1).min(current);
            let active = self.active.load(Ordering::SeqCst);
            if active.saturating_add(slots) <= current {
                if self
                    .active
                    .compare_exchange(active, active + slots, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    return Ok(AdaptivePermit {
                        limiter: self,
                        slots,
                    });
                }
            } else {
                self.backpressured.store(true, Ordering::SeqCst);
                notified.await;
            }
        }
    }

    /// Elapsed time covers the successful attempt only, excluding pacing,
    /// retries and cooldown. Empty/control requests don't train the controller.
    pub fn record_success(&self, bytes: u64, elapsed: Duration) {
        if bytes == 0 || elapsed.is_zero() {
            return;
        }
        let rate = bytes as f64 / elapsed.as_secs_f64();
        let saturated = self.active.load(Ordering::SeqCst) >= self.current()
            || self.backpressured.swap(false, Ordering::SeqCst);
        let mut feedback = self
            .feedback
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let baseline = feedback.bytes_per_second;
        feedback.bytes_per_second = if baseline == 0.0 {
            rate
        } else {
            baseline * 0.8 + rate * 0.2
        };
        if !saturated {
            feedback.healthy = 0;
            feedback.slow = 0;
            return;
        }
        if baseline > 0.0 && rate < baseline * 0.6 {
            feedback.healthy = 0;
            feedback.slow += 1;
            if feedback.slow >= 4 {
                self.adjust(-1);
                feedback.slow = 0;
            }
        } else if baseline == 0.0 || rate >= baseline * 0.85 {
            feedback.slow = 0;
            feedback.healthy += 1;
            if feedback.healthy >= 4 {
                self.adjust(1);
                feedback.healthy = 0;
            }
        } else {
            feedback.healthy = 0;
            feedback.slow = 0;
        }
    }

    pub fn record_error(&self, class: RemoteErrorClass) {
        let mut feedback = self
            .feedback
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        feedback.healthy = 0;
        feedback.slow = 0;
        if class.is_rate_limited() {
            let current = self.current();
            self.set_current((current / 2).max(self.min));
        } else if class.is_retryable() {
            self.adjust(-1);
        }
    }

    fn adjust(&self, delta: i32) {
        let current = self.current() as i32;
        let next = (current + delta).clamp(self.min as i32, self.max as i32) as usize;
        self.set_current(next);
    }

    fn set_current(&self, next: usize) {
        let next = next.clamp(self.min, self.max);
        self.current.store(next, Ordering::SeqCst);
        self.notify.notify_waiters();
    }
}

impl Drop for AdaptivePermit<'_> {
    fn drop(&mut self) {
        self.limiter.active.fetch_sub(self.slots, Ordering::SeqCst);
        self.limiter.notify.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordered_streams_do_not_pay_the_copyto_multistream_divisor() {
        let tuning = TransferTuning::throughput();
        assert_eq!(AdaptiveConcurrency::for_downloads(&tuning).max(), 2);
        assert_eq!(AdaptiveConcurrency::for_streams(&tuning).max(), 8);
        let streams = AdaptiveConcurrency::for_streams(&tuning);
        streams.record_error(RemoteErrorClass::RateLimited);
        assert_eq!(streams.current(), 4);
    }

    #[test]
    fn rate_limits_halve_concurrency_within_bounds() {
        let limiter = AdaptiveConcurrency::new(4, 1, 4);
        limiter.record_error(RemoteErrorClass::RateLimited);
        assert_eq!(limiter.current(), 2);
        limiter.record_error(RemoteErrorClass::RateLimited);
        assert_eq!(limiter.current(), 1);
        limiter.record_error(RemoteErrorClass::RateLimited);
        assert_eq!(limiter.current(), 1);
    }

    #[tokio::test]
    async fn healthy_saturated_throughput_recovers_after_backpressure() {
        let limiter = AdaptiveConcurrency::new(1, 1, 4);
        for _ in 0..4 {
            let _permit = limiter.acquire().await.unwrap();
            limiter.record_success(1024, Duration::from_millis(200));
        }
        assert_eq!(limiter.current(), 2);
        limiter.record_error(RemoteErrorClass::RateLimited);
        assert_eq!(limiter.current(), 1);
        let _permit = limiter.acquire().await.unwrap();
        limiter.record_success(1024, Duration::from_millis(200));
        assert_eq!(limiter.current(), 1); // One success cannot undo a cooldown.
    }

    #[tokio::test]
    async fn variable_pack_durations_at_equal_throughput_do_not_reduce_slots() {
        let limiter = AdaptiveConcurrency::new(4, 1, 4);
        let _permit = limiter.acquire_slots(4).await.unwrap();
        for _ in 0..8 {
            limiter.record_success(1024, Duration::from_millis(1));
            limiter.record_success(1024 * 1024, Duration::from_millis(1024));
        }
        assert_eq!(limiter.current(), 4);
    }

    #[tokio::test]
    async fn bulk_and_single_uploads_share_slots_and_cancelled_waiters_do_not_leak() {
        let limiter = AdaptiveConcurrency::new(4, 1, 4);
        let bulk = limiter.acquire_slots(3).await.unwrap();
        let single = limiter.acquire().await.unwrap();
        assert_eq!(bulk.slots() + single.slots(), 4);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), limiter.acquire())
                .await
                .is_err()
        );
        drop(bulk);
        let replacement = limiter.acquire_slots(3).await.unwrap();
        assert_eq!(replacement.slots(), 3);
        drop(single);
        drop(replacement);
        assert_eq!(limiter.acquire_slots(4).await.unwrap().slots(), 4);
    }

    #[test]
    fn retryable_errors_reduce_one_slot() {
        let limiter = AdaptiveConcurrency::new(3, 1, 4);
        limiter.record_error(RemoteErrorClass::Retryable);
        assert_eq!(limiter.current(), 2);
    }
}
