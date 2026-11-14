use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use noland_rclone_adapter::{RemoteErrorClass, TransferTuning};
use noland_state_core::Result;

/// A remote-error-controlled target with a separate, temporary local-pressure cap.
/// Successful saturated attempts recover the remote target additively; individual
/// pack rates are not comparable congestion signals across widths or workloads.
///
/// Weighted reservations account for all transfers inside a bulk rclone process,
/// as well as individual copyto processes, against the same FIFO admission budget.
pub struct AdaptiveConcurrency {
    min: usize,
    max: usize,
    state: Mutex<State>,
}

struct State {
    current: usize,
    remote_target: usize,
    active: usize,
    epoch: u64,
    backpressured: bool,
    local_pressure: Option<LocalPressure>,
    feedback: Feedback,
    // Granted but not yet polled reservations stay here so cancellation can
    // return their slots. They count as active, not queued, and cannot be stolen.
    waiters: VecDeque<Waiter>,
}

struct Waiter {
    identity: Arc<()>,
    requested: usize,
    granted: Option<usize>,
    waker: Waker,
}

struct LocalPressure {
    quiet_since: Option<Instant>,
}

#[derive(Default)]
struct Feedback {
    healthy: u32,
    last_remote_error: Option<Instant>,
}

const LOCAL_PRESSURE_COOLDOWN: Duration = Duration::from_secs(5);
const REMOTE_RECOVERY_COOLDOWN: Duration = Duration::from_secs(5);
const HEALTHY_SUCCESSES_FOR_RECOVERY: u32 = 4;

impl State {
    fn grant_waiters(&mut self) -> Vec<Waker> {
        let mut free = self.current.saturating_sub(self.active);
        let mut wakers = Vec::new();
        for waiter in &mut self.waiters {
            if free == 0 {
                break;
            }
            if waiter.granted.is_some() {
                continue;
            }
            let slots = waiter.requested.min(free);
            waiter.granted = Some(slots);
            // slots <= current - active, even when current is usize::MAX.
            self.active += slots;
            free -= slots;
            wakers.push(waiter.waker.clone());
        }
        wakers
    }
}

fn wake_waiters(wakers: Vec<Waker>) {
    // Never invoke an executor/user waker while holding the controller lock.
    for waker in wakers {
        waker.wake();
    }
}

struct AcquireSlots<'a> {
    limiter: &'a AdaptiveConcurrency,
    requested: usize,
    identity: Option<Arc<()>>,
}

impl<'a> Future for AcquireSlots<'a> {
    type Output = Result<AdaptivePermit<'a>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let mut state = this.limiter.lock_state();
        let position = if let Some(identity) = &this.identity {
            state
                .waiters
                .iter()
                .position(|waiter| Arc::ptr_eq(&waiter.identity, identity))
                .expect("registered adaptive waiter")
        } else {
            let identity = Arc::new(());
            state.waiters.push_back(Waiter {
                identity: identity.clone(),
                requested: this.requested.max(1),
                granted: None,
                waker: cx.waker().clone(),
            });
            this.identity = Some(identity);
            state.waiters.len() - 1
        };
        if !state.waiters[position].waker.will_wake(cx.waker()) {
            state.waiters[position].waker = cx.waker().clone();
        }
        let wakers = state.grant_waiters();
        let result = if let Some(slots) = state.waiters[position].granted {
            state.waiters.remove(position);
            this.identity = None;
            Poll::Ready(Ok(AdaptivePermit {
                limiter: this.limiter,
                slots,
            }))
        } else {
            state.backpressured = true;
            Poll::Pending
        };
        drop(state);
        wake_waiters(wakers);
        result
    }
}

impl Drop for AcquireSlots<'_> {
    fn drop(&mut self) {
        let Some(identity) = self.identity.take() else {
            return;
        };
        let wakers = {
            let mut state = self.limiter.lock_state();
            let position = state
                .waiters
                .iter()
                .position(|waiter| Arc::ptr_eq(&waiter.identity, &identity))
                .expect("registered adaptive waiter");
            let waiter = state.waiters.remove(position).unwrap();
            if let Some(slots) = waiter.granted {
                state.active -= slots;
            }
            state.grant_waiters()
        };
        wake_waiters(wakers);
    }
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
        let released = self.slots - next;
        self.slots = next;
        self.limiter.release(released);
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
        // Generic backends have no rclone multistream multiplier. A backend
        // that starts multiple connections must acquire that many slots itself.
        let max = tuning.max_parallel_downloads.max(1);
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
            state: Mutex::new(State {
                current: initial,
                remote_target: initial,
                active: 0,
                epoch: 0,
                backpressured: false,
                local_pressure: None,
                feedback: Feedback::default(),
                waiters: VecDeque::new(),
            }),
        })
    }

    pub fn current(&self) -> usize {
        self.lock_state().current
    }

    pub fn active(&self) -> usize {
        self.lock_state().active
    }

    /// Requests still waiting for capacity, excluding reserved, unpolled grants.
    pub fn queued(&self) -> usize {
        self.lock_state()
            .waiters
            .iter()
            .filter(|waiter| waiter.granted.is_none())
            .count()
    }

    /// Changes only when the effective admission limit changes.
    pub fn epoch(&self) -> u64 {
        self.lock_state().epoch
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

    /// In first-poll FIFO order, grant min(requested.max(1), free capacity).
    /// New arrivals never bypass older requests, even before those requests are
    /// woken/polled. A group does not wait for its entire preferred width.
    /// The caller must configure backend concurrency from `permit.slots()`.
    pub async fn acquire_slots(&self, requested: usize) -> Result<AdaptivePermit<'_>> {
        AcquireSlots {
            limiter: self,
            requested,
            identity: None,
        }
        .await
    }

    /// Elapsed time covers the successful attempt only, excluding pacing,
    /// retries and cooldown. Empty/control requests don't train the controller.
    /// Four healthy saturated attempts recover one remote slot after error
    /// cooldown; slower historical per-pack rates never reduce the target.
    pub fn record_success(&self, bytes: u64, elapsed: Duration) {
        self.record_success_at(bytes, elapsed, Instant::now());
    }

    fn record_success_at(&self, bytes: u64, elapsed: Duration, now: Instant) {
        if bytes == 0 || elapsed.is_zero() {
            return;
        }
        let wakers = {
            let mut state = self.lock_state();
            let saturated = state.active >= state.current || state.backpressured;
            state.backpressured = false;
            if state.local_pressure.is_some()
                || !saturated
                || state.feedback.last_remote_error.is_some_and(|last| {
                    now.saturating_duration_since(last) < REMOTE_RECOVERY_COOLDOWN
                })
            {
                state.feedback.healthy = 0;
                return;
            }
            state.feedback.healthy += 1;
            if state.feedback.healthy < HEALTHY_SUCCESSES_FOR_RECOVERY {
                return;
            }
            state.feedback.healthy = 0;
            state.remote_target = state.remote_target.saturating_add(1).min(self.max);
            self.refresh_limit(&mut state)
        };
        wake_waiters(wakers);
    }

    /// Local disk pressure limits new admissions, not running applications or
    /// active children. The cap is max(min, ceil(max / 2)), independent of the
    /// remote target. Repeated true observations do not compound this cap.
    /// A false observation starts a quiet period; a later false observation at
    /// least five seconds later clears it. Time or completions alone cannot
    /// expire stale pressure, and another true observation resets the quiet period.
    pub fn observe_local_pressure(&self, pressured: bool) {
        self.observe_local_pressure_at(pressured, Instant::now());
    }

    fn observe_local_pressure_at(&self, pressured: bool, now: Instant) {
        let wakers = {
            let mut state = self.lock_state();
            if pressured {
                state.local_pressure = Some(LocalPressure { quiet_since: None });
                state.feedback.healthy = 0;
            } else if let Some(pressure) = &mut state.local_pressure {
                let quiet_since = pressure.quiet_since.get_or_insert(now);
                if now.saturating_duration_since(*quiet_since) >= LOCAL_PRESSURE_COOLDOWN {
                    state.local_pressure = None;
                    state.feedback.healthy = 0;
                }
            }
            self.refresh_limit(&mut state)
        };
        wake_waiters(wakers);
    }

    pub fn record_error(&self, class: RemoteErrorClass) {
        self.record_error_at(class, Instant::now());
    }

    fn record_error_at(&self, class: RemoteErrorClass, now: Instant) {
        let wakers = {
            let mut state = self.lock_state();
            state.feedback.healthy = 0;
            if class.is_retryable() {
                state.feedback.last_remote_error = Some(now);
                state.remote_target = if class.is_rate_limited() {
                    state.remote_target / 2
                } else {
                    state.remote_target.saturating_sub(1)
                }
                .clamp(self.min, self.max);
            }
            self.refresh_limit(&mut state)
        };
        wake_waiters(wakers);
    }

    fn lock_state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn refresh_limit(&self, state: &mut State) -> Vec<Waker> {
        let cap = if state.local_pressure.is_some() {
            self.max.div_ceil(2).max(self.min)
        } else {
            self.max
        };
        let next = state.remote_target.min(cap).clamp(self.min, self.max);
        if next == state.current {
            return Vec::new();
        }
        state.current = next;
        state.epoch = state.epoch.wrapping_add(1);
        // No throughput/recovery evidence survives a different admission context.
        state.feedback.healthy = 0;
        state.backpressured = false;
        state.grant_waiters()
    }

    fn release(&self, slots: usize) {
        if slots == 0 {
            return;
        }
        let wakers = {
            let mut state = self.lock_state();
            state.active -= slots;
            state.grant_waiters()
        };
        wake_waiters(wakers);
    }
}

impl Drop for AdaptivePermit<'_> {
    fn drop(&mut self) {
        self.limiter.release(self.slots);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Wake;

    fn poll_once<F: Future + ?Sized>(future: Pin<&mut F>) -> Poll<F::Output> {
        future.poll(&mut Context::from_waker(Waker::noop()))
    }

    fn ready<F: Future + ?Sized>(future: Pin<&mut F>) -> F::Output {
        match poll_once(future) {
            Poll::Ready(output) => output,
            Poll::Pending => panic!("expected an immediately ready future"),
        }
    }

    fn acquire_now(limiter: &AdaptiveConcurrency, width: usize) -> AdaptivePermit<'_> {
        let mut future = Box::pin(limiter.acquire_slots(width));
        ready(future.as_mut()).unwrap()
    }

    fn sample_at(limiter: &AdaptiveConcurrency, now: Instant) {
        limiter.record_success_at(1024, Duration::from_millis(10), now);
    }

    fn directional_limiters(tuning: &TransferTuning) -> [Arc<AdaptiveConcurrency>; 2] {
        [
            AdaptiveConcurrency::for_uploads(tuning),
            AdaptiveConcurrency::for_downloads(tuning),
        ]
    }

    #[derive(Default)]
    struct WakeCounter(AtomicUsize);

    impl Wake for WakeCounter {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn repeated_local_pressure_caps_sixteen_at_eight_without_recursive_halving() {
        let tuning = TransferTuning {
            max_parallel_uploads: 16,
            max_parallel_downloads: 16,
            ..Default::default()
        };
        let now = Instant::now();
        for limiter in directional_limiters(&tuning) {
            let busy = acquire_now(&limiter, 16);
            for tick in 0..128 {
                limiter.observe_local_pressure_at(true, now + Duration::from_secs(tick * 6));
                sample_at(&limiter, now + Duration::from_secs(tick * 6 + 1));
                assert_eq!(limiter.current(), 8);
                assert_eq!(limiter.active(), 16, "admitted children are grandfathered");
                assert_eq!(limiter.max(), 16);
                assert_eq!(limiter.epoch(), 1);
            }
            assert_eq!(limiter.lock_state().remote_target, 16);
            drop(busy);
            assert_eq!(acquire_now(&limiter, 16).slots(), 8);
        }
    }

    #[test]
    fn fresh_false_quiet_period_restores_queued_capacity_without_a_completion() {
        let limiter = AdaptiveConcurrency::new(16, 1, 16);
        let now = Instant::now();
        limiter.observe_local_pressure_at(true, now);
        let busy = acquire_now(&limiter, 8);
        let wake_counter = Arc::new(WakeCounter::default());
        let waker = Waker::from(wake_counter.clone());
        let mut waiting = Box::pin(limiter.acquire_slots(usize::MAX));
        assert!(waiting
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending());
        assert_eq!(limiter.queued(), 1);
        limiter.observe_local_pressure_at(false, now + Duration::from_secs(1));
        limiter.observe_local_pressure_at(false, now + Duration::from_millis(5_999));
        assert_eq!(limiter.current(), 8);
        assert_eq!(limiter.queued(), 1);
        limiter.observe_local_pressure_at(false, now + Duration::from_secs(6));
        assert_eq!(limiter.current(), 16);
        assert_eq!(limiter.epoch(), 2);
        assert_eq!(limiter.active(), 16, "the old waiter is already reserved");
        assert_eq!(limiter.queued(), 0);
        assert_eq!(wake_counter.0.load(Ordering::SeqCst), 1);
        let recovered = ready(waiting.as_mut()).unwrap();
        assert_eq!(recovered.slots(), 8);
        drop(busy);
        drop(recovered);
        assert_eq!(limiter.active(), 0);
    }

    #[test]
    fn stale_true_and_a_single_late_false_cannot_expire_pressure() {
        let limiter = AdaptiveConcurrency::new(16, 1, 16);
        let now = Instant::now();
        limiter.observe_local_pressure_at(true, now);
        for _ in 0..64 {
            sample_at(&limiter, now + Duration::from_secs(60));
        }
        assert_eq!(limiter.current(), 8);
        limiter.observe_local_pressure_at(false, now + Duration::from_secs(100));
        assert_eq!(
            limiter.current(),
            8,
            "fresh false starts, not skips, quiet time"
        );
        limiter.observe_local_pressure_at(false, now + Duration::from_millis(104_999));
        assert_eq!(limiter.current(), 8);
        limiter.observe_local_pressure_at(true, now + Duration::from_secs(105));
        limiter.observe_local_pressure_at(false, now + Duration::from_secs(106));
        limiter.observe_local_pressure_at(false, now + Duration::from_millis(110_999));
        assert_eq!(limiter.current(), 8, "another true resets the quiet period");
        limiter.observe_local_pressure_at(false, now + Duration::from_secs(111));
        assert_eq!(limiter.current(), 16);
    }

    #[test]
    fn rate_limit_uses_remote_target_once_and_survives_local_pressure_clear() {
        let now = Instant::now();
        for remote_first in [false, true] {
            let limiter = AdaptiveConcurrency::new(16, 1, 16);
            let _busy = acquire_now(&limiter, 16);
            if remote_first {
                limiter.record_error_at(RemoteErrorClass::RateLimited, now);
                limiter.observe_local_pressure_at(true, now);
            } else {
                limiter.observe_local_pressure_at(true, now);
                limiter.record_error_at(RemoteErrorClass::RateLimited, now);
            }
            assert_eq!(
                limiter.current(),
                8,
                "local and remote penalties do not multiply"
            );
            assert_eq!(limiter.epoch(), 1);
            limiter.observe_local_pressure_at(false, now + Duration::from_secs(1));
            limiter.observe_local_pressure_at(false, now + Duration::from_secs(6));
            assert_eq!(
                limiter.current(),
                8,
                "clearing local pressure cannot undo a 429"
            );
            assert_eq!(limiter.lock_state().remote_target, 8);
            assert_eq!(limiter.epoch(), 1);
            for _ in 0..3 {
                sample_at(&limiter, now + Duration::from_secs(6));
            }
            assert_eq!(limiter.current(), 8);
            sample_at(&limiter, now + Duration::from_secs(6));
            assert_eq!(
                limiter.current(),
                9,
                "remote recovery remains additive and bounded"
            );
        }
    }

    #[test]
    fn retryable_errors_reduce_remote_target_not_the_local_cap() {
        let limiter = AdaptiveConcurrency::new(16, 1, 16);
        let now = Instant::now();
        limiter.observe_local_pressure_at(true, now);
        limiter.record_error_at(RemoteErrorClass::Retryable, now);
        assert_eq!(limiter.current(), 8);
        assert_eq!(limiter.lock_state().remote_target, 15);
        limiter.observe_local_pressure_at(false, now + Duration::from_secs(1));
        limiter.observe_local_pressure_at(false, now + Duration::from_secs(6));
        assert_eq!(limiter.current(), 15);
        limiter.record_error_at(RemoteErrorClass::Retryable, now + Duration::from_secs(6));
        assert_eq!(limiter.current(), 14);
    }

    #[test]
    fn epoch_tracks_only_effective_limit_changes() {
        let limiter = AdaptiveConcurrency::new(16, 1, 16);
        let now = Instant::now();
        assert_eq!(limiter.epoch(), 0);
        limiter.observe_local_pressure_at(true, now);
        assert_eq!(limiter.epoch(), 1);
        limiter.observe_local_pressure_at(true, now + Duration::from_secs(10));
        limiter.record_error_at(RemoteErrorClass::Retryable, now);
        assert_eq!(
            limiter.epoch(),
            1,
            "remote 15 is hidden behind the cap of 8"
        );
        limiter.record_error_at(RemoteErrorClass::RateLimited, now);
        assert_eq!(limiter.current(), 7);
        assert_eq!(limiter.epoch(), 2);
        limiter.observe_local_pressure_at(false, now + Duration::from_secs(11));
        limiter.observe_local_pressure_at(false, now + Duration::from_secs(16));
        assert_eq!(limiter.current(), 7);
        assert_eq!(limiter.epoch(), 2);
        limiter.record_error_at(RemoteErrorClass::Authentication, now);
        let permit = acquire_now(&limiter, 7);
        drop(permit);
        assert_eq!(
            limiter.epoch(),
            2,
            "admission and non-congestion errors do not change it"
        );
    }

    #[test]
    fn download_objects_and_internal_connections_share_the_same_raw_budget() {
        let tuning = TransferTuning {
            max_parallel_downloads: 5,
            rclone_download_streams: 4,
            ..Default::default()
        };
        let limiter = AdaptiveConcurrency::for_downloads(&tuning);
        assert_eq!(limiter.max(), 5);
        let copy = acquire_now(&limiter, 4);
        let ordered = acquire_now(&limiter, 1);
        assert_eq!(copy.slots() + ordered.slots(), 5);
        assert_eq!(limiter.active(), 5);
        drop(copy);
        let partial = acquire_now(&limiter, 5);
        assert_eq!(partial.slots(), 4);
        limiter.observe_local_pressure(true);
        assert_eq!(limiter.current(), 3);
        assert_eq!(
            limiter.active(),
            5,
            "pressure never cancels admitted children"
        );
        drop(partial);
        drop(ordered);
        assert_eq!(limiter.active(), 0);
        assert_eq!(acquire_now(&limiter, 5).slots(), 3);
    }

    #[test]
    fn large_automatic_ceilings_do_not_overflow_adaptive_adjustment() {
        let limiter = AdaptiveConcurrency::new(usize::MAX, 1, usize::MAX);
        let now = Instant::now();
        limiter.record_error_at(RemoteErrorClass::Retryable, now);
        assert_eq!(limiter.current(), usize::MAX - 1);
        let mut busy = acquire_now(&limiter, usize::MAX);
        for _ in 0..8 {
            sample_at(&limiter, now + Duration::from_secs(5));
        }
        assert_eq!(limiter.current(), usize::MAX);
        let last = acquire_now(&limiter, usize::MAX);
        assert_eq!(last.slots(), 1);
        assert_eq!(limiter.active(), usize::MAX);
        let mut waiting = Box::pin(limiter.acquire_slots(usize::MAX));
        assert!(poll_once(waiting.as_mut()).is_pending());
        drop(last);
        assert_eq!(limiter.active(), usize::MAX);
        drop(waiting);
        assert_eq!(limiter.active(), usize::MAX - 1);
        busy.retain_slots(0);
        assert_eq!(busy.slots(), 1);
        assert_eq!(limiter.active(), 1);
        drop(busy);
        assert_eq!(limiter.active(), 0);
    }

    #[test]
    fn pressure_half_ceiling_is_overflow_safe_and_does_not_compound_at_usize_max() {
        let limiter = AdaptiveConcurrency::new(usize::MAX, 1, usize::MAX);
        let now = Instant::now();
        for second in 0..16 {
            limiter.observe_local_pressure_at(true, now + Duration::from_secs(second * 10));
            assert_eq!(limiter.current(), usize::MAX / 2 + 1);
        }
        limiter.record_error_at(
            RemoteErrorClass::RateLimited,
            now + Duration::from_secs(150),
        );
        assert_eq!(limiter.current(), usize::MAX / 2);
        limiter.observe_local_pressure_at(false, now + Duration::from_secs(151));
        limiter.observe_local_pressure_at(false, now + Duration::from_secs(156));
        assert_eq!(limiter.current(), usize::MAX / 2);
    }

    #[test]
    fn minimum_one_and_explicit_minimum_are_preserved_under_all_pressure() {
        let limiter = AdaptiveConcurrency::new(0, 0, 0);
        let now = Instant::now();
        for _ in 0..16 {
            limiter.observe_local_pressure_at(true, now);
            limiter.record_error_at(RemoteErrorClass::RateLimited, now);
            limiter.record_error_at(RemoteErrorClass::Retryable, now);
        }
        assert_eq!((limiter.min(), limiter.current(), limiter.max()), (1, 1, 1));
        assert_eq!(acquire_now(&limiter, 0).slots(), 1);
        let limiter = AdaptiveConcurrency::new(16, 10, 16);
        let busy = acquire_now(&limiter, 16);
        limiter.observe_local_pressure_at(true, now);
        limiter.record_error_at(RemoteErrorClass::RateLimited, now);
        assert_eq!(limiter.current(), 10);
        assert_eq!(limiter.active(), 16);
        drop(busy);
        assert_eq!(acquire_now(&limiter, usize::MAX).slots(), 10);
    }

    #[test]
    fn ordered_streams_do_not_pay_the_copyto_multistream_divisor() {
        let tuning = TransferTuning::throughput();
        assert_eq!(AdaptiveConcurrency::for_downloads(&tuning).max(), 8);
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

    #[test]
    fn healthy_saturated_successes_recover_additively_after_error_cooldown() {
        let limiter = AdaptiveConcurrency::new(1, 1, 4);
        let now = Instant::now();
        let _permit = acquire_now(&limiter, 1);
        for _ in 0..4 {
            sample_at(&limiter, now);
        }
        assert_eq!(limiter.current(), 2);
        limiter.record_error_at(RemoteErrorClass::RateLimited, now);
        assert_eq!(limiter.current(), 1);
        for _ in 0..64 {
            sample_at(&limiter, now + Duration::from_millis(4_999));
        }
        assert_eq!(
            limiter.current(),
            1,
            "successes during cooldown are not banked"
        );
        for _ in 0..3 {
            sample_at(&limiter, now + Duration::from_secs(5));
        }
        assert_eq!(limiter.current(), 1);
        sample_at(&limiter, now + Duration::from_secs(5));
        assert_eq!(limiter.current(), 2);
    }

    #[test]
    fn historical_individual_pack_rate_decreases_never_reduce_global_slots() {
        let limiter = AdaptiveConcurrency::new(16, 1, 16);
        let _permit = acquire_now(&limiter, 16);
        let now = Instant::now();
        limiter.record_success_at(u64::MAX, Duration::from_nanos(1), now);
        for seconds in 1..128 {
            limiter.record_success_at(1, Duration::from_secs(seconds), now);
            assert_eq!(limiter.current(), 16);
        }
        assert_eq!(limiter.epoch(), 0);
    }

    #[test]
    fn recovery_evidence_is_reset_after_each_effective_limit_change() {
        let limiter = AdaptiveConcurrency::new(4, 1, 8);
        let _permit = acquire_now(&limiter, 4);
        let now = Instant::now();
        for _ in 0..3 {
            sample_at(&limiter, now);
        }
        assert_eq!(limiter.lock_state().feedback.healthy, 3);
        limiter.record_error_at(RemoteErrorClass::RateLimited, now);
        assert_eq!(limiter.current(), 2);
        for _ in 0..3 {
            sample_at(&limiter, now + Duration::from_secs(5));
        }
        assert_eq!(limiter.current(), 2);
        sample_at(&limiter, now + Duration::from_secs(5));
        assert_eq!(limiter.current(), 3);
        assert_eq!(limiter.lock_state().feedback.healthy, 0);
        assert_eq!(limiter.epoch(), 2);
    }

    #[test]
    fn control_requests_and_unsaturated_successes_do_not_train_recovery() {
        let limiter = AdaptiveConcurrency::new(1, 1, 4);
        let now = Instant::now();
        for _ in 0..16 {
            sample_at(&limiter, now);
        }
        let _permit = acquire_now(&limiter, 1);
        for _ in 0..16 {
            limiter.record_success_at(0, Duration::from_secs(1), now);
            limiter.record_success_at(1024, Duration::ZERO, now);
        }
        assert_eq!(limiter.current(), 1);
        assert_eq!(limiter.lock_state().feedback.healthy, 0);
    }

    #[test]
    fn older_waiters_win_before_continuous_new_arrivals_in_both_directions() {
        let tuning = TransferTuning {
            max_parallel_uploads: 1,
            max_parallel_downloads: 1,
            ..Default::default()
        };
        for limiter in directional_limiters(&tuning) {
            let mut busy = acquire_now(&limiter, 1);
            let mut waiting = VecDeque::new();
            let mut first = Box::pin(limiter.acquire_slots(1));
            assert!(poll_once(first.as_mut()).is_pending());
            waiting.push_back((0, first));
            for arrival in 1..=256 {
                let mut newcomer = Box::pin(limiter.acquire_slots(1));
                assert!(poll_once(newcomer.as_mut()).is_pending());
                waiting.push_back((arrival, newcomer));
                drop(busy);
                // Deliberately poll the newcomer before the old, woken request.
                assert!(poll_once(waiting.back_mut().unwrap().1.as_mut()).is_pending());
                let (identity, mut oldest) = waiting.pop_front().unwrap();
                assert_eq!(identity, arrival - 1);
                busy = ready(oldest.as_mut()).unwrap();
                assert_eq!(busy.slots(), 1);
                assert_eq!(limiter.active(), 1);
                assert_eq!(limiter.queued(), 1);
            }
            drop(busy);
            let (_, mut last) = waiting.pop_front().unwrap();
            drop(ready(last.as_mut()).unwrap());
            assert_eq!(limiter.active(), 0);
            assert_eq!(limiter.queued(), 0);
        }
    }

    #[test]
    fn weighted_fifo_grants_partial_width_without_newcomer_bypass() {
        let limiter = AdaptiveConcurrency::new(5, 1, 5);
        let mut busy = acquire_now(&limiter, 5);
        let mut first = Box::pin(limiter.acquire_slots(4));
        let mut second = Box::pin(limiter.acquire_slots(3));
        let mut single = Box::pin(limiter.acquire_slots(1));
        for future in [&mut first, &mut second, &mut single] {
            assert!(poll_once(future.as_mut()).is_pending());
        }
        busy.retain_slots(3);
        assert_eq!(limiter.queued(), 2);
        assert!(poll_once(second.as_mut()).is_pending());
        let first = ready(first.as_mut()).unwrap();
        assert_eq!(
            first.slots(),
            2,
            "the head takes free slots without waiting for four"
        );
        busy.retain_slots(1);
        let second = ready(second.as_mut()).unwrap();
        assert_eq!(second.slots(), 2);
        assert_eq!(limiter.active(), 5);
        let mut newcomer = Box::pin(limiter.acquire_slots(usize::MAX));
        assert!(poll_once(newcomer.as_mut()).is_pending());
        drop(first);
        let single = ready(single.as_mut()).unwrap();
        let newcomer = ready(newcomer.as_mut()).unwrap();
        assert_eq!(single.slots(), 1);
        assert_eq!(
            newcomer.slots(),
            1,
            "only capacity left after the older single"
        );
        assert_eq!(limiter.active(), 5);
        drop(busy);
        drop(second);
        drop(single);
        drop(newcomer);
        assert_eq!(limiter.active(), 0);
    }

    #[test]
    fn cancelling_head_middle_and_reserved_unpolled_grants_does_not_leak() {
        let limiter = AdaptiveConcurrency::new(3, 1, 3);
        let mut busy = acquire_now(&limiter, 3);
        let mut head = Box::pin(limiter.acquire_slots(usize::MAX));
        let mut middle = Box::pin(limiter.acquire_slots(1));
        let mut tail = Box::pin(limiter.acquire_slots(usize::MAX));
        for future in [&mut head, &mut middle, &mut tail] {
            assert!(poll_once(future.as_mut()).is_pending());
        }
        drop(middle);
        assert_eq!(limiter.queued(), 2);
        drop(head);
        assert_eq!(limiter.queued(), 1);
        busy.retain_slots(1);
        assert_eq!(limiter.active(), 3);
        assert_eq!(
            limiter.queued(),
            0,
            "tail now owns an unpolled two-slot grant"
        );
        let mut replacement = Box::pin(limiter.acquire_slots(usize::MAX));
        assert!(poll_once(replacement.as_mut()).is_pending());
        drop(tail);
        assert_eq!(
            limiter.active(),
            3,
            "cancelled grant passes directly to the next waiter"
        );
        assert_eq!(limiter.queued(), 0);
        let replacement = ready(replacement.as_mut()).unwrap();
        assert_eq!(replacement.slots(), 2);
        drop(replacement);
        assert_eq!(limiter.active(), 1);
        drop(busy);
        assert_eq!(limiter.active(), 0);
        assert_eq!(acquire_now(&limiter, 3).slots(), 3);
    }

    #[test]
    fn cancelling_already_reserved_head_and_middle_wakes_the_tail() {
        let limiter = AdaptiveConcurrency::new(3, 1, 3);
        let busy = acquire_now(&limiter, 3);
        let mut head = Box::pin(limiter.acquire_slots(1));
        let mut middle = Box::pin(limiter.acquire_slots(2));
        let mut tail = Box::pin(limiter.acquire_slots(usize::MAX));
        for future in [&mut head, &mut middle, &mut tail] {
            assert!(poll_once(future.as_mut()).is_pending());
        }
        drop(busy);
        assert_eq!(limiter.active(), 3);
        assert_eq!(limiter.queued(), 1);
        drop(middle);
        assert_eq!(limiter.queued(), 0);
        drop(head);
        assert_eq!(limiter.active(), 2);
        let tail = ready(tail.as_mut()).unwrap();
        assert_eq!(tail.slots(), 2, "a partial grant is not silently enlarged");
        let spare = acquire_now(&limiter, 3);
        assert_eq!(spare.slots(), 1);
        drop(tail);
        drop(spare);
        assert_eq!(limiter.active(), 0);
    }

    #[test]
    fn repolling_updates_the_waker_and_dropping_ready_outputs_returns_capacity() {
        let limiter = AdaptiveConcurrency::new(1, 1, 1);
        let busy = acquire_now(&limiter, 1);
        let old = Arc::new(WakeCounter::default());
        let new = Arc::new(WakeCounter::default());
        let mut waiting = Box::pin(limiter.acquire());
        for counter in [&old, &new] {
            let waker = Waker::from(counter.clone());
            assert!(waiting
                .as_mut()
                .poll(&mut Context::from_waker(&waker))
                .is_pending());
        }
        assert_eq!(
            limiter.queued(),
            1,
            "repolling does not duplicate a request"
        );
        drop(busy);
        assert_eq!(old.0.load(Ordering::SeqCst), 0);
        assert_eq!(new.0.load(Ordering::SeqCst), 1);
        drop(ready(waiting.as_mut()).unwrap());
        drop(waiting);
        assert_eq!(limiter.active(), 0);
        assert_eq!(limiter.queued(), 0);
    }

    #[test]
    fn unbounded_widths_are_bounded_and_retain_keeps_one_process_slot_until_drop() {
        let tuning = TransferTuning {
            max_parallel_uploads: 16,
            max_parallel_downloads: 16,
            ..Default::default()
        };
        for limiter in directional_limiters(&tuning) {
            let mut busy = acquire_now(&limiter, usize::MAX);
            assert_eq!(busy.slots(), 16);
            let mut waiting = Box::pin(limiter.acquire_slots(usize::MAX));
            assert!(poll_once(waiting.as_mut()).is_pending());
            limiter.observe_local_pressure(true);
            assert_eq!(limiter.active(), 16);
            busy.retain_slots(0);
            assert_eq!(busy.slots(), 1);
            assert_eq!(limiter.active(), 8);
            let mut partial = ready(waiting.as_mut()).unwrap();
            assert_eq!(partial.slots(), 7);
            partial.retain_slots(usize::MAX);
            assert_eq!(partial.slots(), 7, "retain cannot add slots");
            partial.retain_slots(0);
            assert_eq!(partial.slots(), 1);
            assert_eq!(limiter.active(), 2);
            drop(partial);
            assert_eq!(limiter.active(), 1);
            drop(busy);
            assert_eq!(limiter.active(), 0);
        }
    }

    #[test]
    fn deterministic_contention_and_cancellation_stress_for_uploads_and_downloads() {
        let tuning = TransferTuning {
            max_parallel_uploads: 16,
            max_parallel_downloads: 11,
            ..Default::default()
        };
        for limiter in directional_limiters(&tuning) {
            let mut running = VecDeque::from([acquire_now(&limiter, usize::MAX)]);
            let mut waiting = VecDeque::new();
            for turn in 0..512 {
                let width = match turn % 5 {
                    0 => 0,
                    1 => 1,
                    2 => limiter.max() / 2,
                    3 => limiter.max(),
                    _ => usize::MAX,
                };
                let mut next = Box::pin(limiter.acquire_slots(width));
                match poll_once(next.as_mut()) {
                    Poll::Ready(result) => running.push_back(result.unwrap()),
                    Poll::Pending => waiting.push_back(next),
                }
                if turn % 7 == 0 && waiting.len() > 3 {
                    // Cancel both pending and already-reserved requests, depending
                    // on the interleaving, before their next poll.
                    drop(waiting.remove(waiting.len() / 2));
                }
                if turn % 3 == 0 {
                    if let Some(permit) = running.front_mut() {
                        permit.retain_slots(0);
                    }
                }
                if turn % 2 == 0 {
                    drop(running.pop_front());
                }
                if let Some(oldest) = waiting.front_mut() {
                    if let Poll::Ready(result) = poll_once(oldest.as_mut()) {
                        running.push_back(result.unwrap());
                        waiting.pop_front();
                    }
                }
                let state = limiter.lock_state();
                let held = running.iter().map(AdaptivePermit::slots).sum::<usize>();
                let reserved = state
                    .waiters
                    .iter()
                    .filter_map(|waiter| waiter.granted)
                    .sum::<usize>();
                assert_eq!(state.active, held + reserved);
                assert!(state.active <= limiter.max());
                assert_eq!(state.waiters.len(), waiting.len());
            }
            while let Some(oldest) = waiting.front_mut() {
                drop(running.pop_front());
                match poll_once(oldest.as_mut()) {
                    Poll::Ready(result) => {
                        running.push_back(result.unwrap());
                        waiting.pop_front();
                    }
                    Poll::Pending => assert!(!running.is_empty(), "no stranded queue head"),
                }
            }
            drop(running);
            assert_eq!(limiter.active(), 0);
            assert_eq!(limiter.queued(), 0);
            assert!(limiter.lock_state().waiters.is_empty());
            assert_eq!(acquire_now(&limiter, usize::MAX).slots(), limiter.max());
        }
    }
}
