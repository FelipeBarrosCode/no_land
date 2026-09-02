use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use noland_rclone_adapter::{RemoteErrorClass, TransferTuning};
use noland_state_core::{Result, StateError};
use tokio::sync::{Notify, Semaphore};

/// Raises and lowers in-flight transfer slots from observed latency and errors.
///
/// The underlying semaphore is sized to the profile maximum. Logical concurrency
/// sits between that maximum and a floor of 1 (or the gameplay-safe cap).
pub struct AdaptiveConcurrency {
    min: usize,
    max: usize,
    current: AtomicUsize,
    active: AtomicUsize,
    samples: AtomicU32,
    consecutive_errors: AtomicU32,
    ewma_latency_ms: std::sync::Mutex<f64>,
    notify: Notify,
    semaphore: Semaphore,
}

pub struct AdaptivePermit<'a> {
    limiter: &'a AdaptiveConcurrency,
    _permit: tokio::sync::SemaphorePermit<'a>,
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
        let max = tuning.max_parallel_downloads.max(1);
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
            samples: AtomicU32::new(0),
            consecutive_errors: AtomicU32::new(0),
            ewma_latency_ms: std::sync::Mutex::new(0.0),
            notify: Notify::new(),
            semaphore: Semaphore::new(max),
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
        loop {
            let current = self.current();
            let active = self.active.load(Ordering::SeqCst);
            if active < current {
                if self
                    .active
                    .compare_exchange(active, active + 1, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    let permit = self
                        .semaphore
                        .acquire()
                        .await
                        .map_err(|error| StateError::Storage(error.to_string()))?;
                    return Ok(AdaptivePermit {
                        limiter: self,
                        _permit: permit,
                    });
                }
            } else {
                self.notify.notified().await;
            }
        }
    }

    pub fn record_success(&self, latency: Duration) {
        self.consecutive_errors.store(0, Ordering::SeqCst);
        let latency_ms = latency.as_secs_f64() * 1_000.0;
        let samples = self
            .samples
            .fetch_add(1, Ordering::SeqCst)
            .saturating_add(1);
        let ewma = {
            let mut ewma = self
                .ewma_latency_ms
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if *ewma <= 0.0 {
                *ewma = latency_ms;
            } else {
                *ewma = (0.2 * latency_ms) + (0.8 * *ewma);
            }
            *ewma
        };
        if samples < 4 {
            return;
        }
        if latency_ms < ewma * 0.5 {
            self.adjust(1);
        } else if latency_ms > ewma * 2.5 {
            self.adjust(-1);
        }
    }

    pub fn record_error(&self, class: RemoteErrorClass) {
        self.consecutive_errors.fetch_add(1, Ordering::SeqCst);
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
        self.limiter.active.fetch_sub(1, Ordering::SeqCst);
        self.limiter.notify.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn healthy_low_latency_raises_workers_after_warmup() {
        let limiter = AdaptiveConcurrency::new(1, 1, 4);
        for _ in 0..4 {
            limiter.record_success(Duration::from_millis(200));
        }
        for _ in 0..8 {
            limiter.record_success(Duration::from_millis(10));
        }
        assert!(limiter.current() > 1);
        assert!(limiter.current() <= limiter.max());
    }

    #[test]
    fn retryable_errors_reduce_one_slot() {
        let limiter = AdaptiveConcurrency::new(3, 1, 4);
        limiter.record_error(RemoteErrorClass::Retryable);
        assert_eq!(limiter.current(), 2);
    }
}
