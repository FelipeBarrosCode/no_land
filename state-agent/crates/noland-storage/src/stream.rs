//! Network-only stall detection. Decoding/publication waits must not trigger
//! retries or train the network controller as if the provider were slow.
use std::collections::VecDeque;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::{DownloadObserver, DownloadStreamConsumer};
use noland_state_core::{Result, StateError};

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct StreamTransferDiagnostics {
    /// Shared download admission sampled at phase boundaries, not socket counts.
    pub worker_ceiling: usize,
    pub effective_worker_limit: usize,
    pub reserved_transfer_slots: usize,
    pub queued_transfer_requests: usize,
    pub controller_epoch: u64,
    pub attempt: u32,
    pub attempt_bytes: u64,
    pub received_bytes: u64,
    pub network_wait_us: u64,
    pub consumer_us: u64,
    pub process_exit_wait_us: u64,
    pub limiter_wait_us: u64,
    pub retry_wait_us: u64,
    pub idle_restarts: u32,
    pub slow_restarts: u32,
    pub exit_restarts: u32,
    pub phase: String,
}

pub(crate) fn micros(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}

#[derive(Clone, Copy)]
pub(crate) struct StreamPolicy {
    pub idle_timeout: Duration,
    pub exit_timeout: Duration,
    pub slow_window: Duration,
    pub observation_interval: Duration,
    pub io_pressure: fn() -> bool,
}

impl Default for StreamPolicy {
    fn default() -> Self {
        Self {
            idle_timeout: Duration::from_secs(60),
            exit_timeout: Duration::from_secs(30),
            slow_window: Duration::from_secs(30),
            observation_interval: Duration::from_secs(1),
            io_pressure,
        }
    }
}

pub(crate) fn io_pressure() -> bool {
    std::fs::read_to_string("/proc/pressure/io")
        .ok()
        .is_some_and(|text| {
            text.lines()
                .filter(|line| line.starts_with("some "))
                .any(|line| {
                    line.split_whitespace()
                        .find_map(|part| part.strip_prefix("avg10="))
                        .and_then(|value| value.parse::<f64>().ok())
                        .is_some_and(|value| value >= 5.0)
                })
        })
}

/// Actual reserved network slots, not the configured worker ceiling. A transfer
/// can occupy several slots; without slots outside its own width it has no
/// independent live peers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PeerContext {
    pub active_slots: usize,
    pub transfer_slots: usize,
    pub generation: u64,
}

impl PeerContext {
    pub fn new(active_slots: usize, transfer_slots: usize, generation: u64) -> Self {
        let active_slots = active_slots.max(1);
        Self {
            active_slots,
            transfer_slots: transfer_slots.max(1).min(active_slots),
            generation,
        }
    }

    fn normalized(self) -> Self {
        Self::new(self.active_slots, self.transfer_slots, self.generation)
    }

    fn has_independent_peers(self) -> bool {
        self.active_slots > self.transfer_slots
    }
}

const PEER_HISTORY_LIMIT: usize = 16;
const PEER_HISTORY_TTL: Duration = Duration::from_secs(120);
const MIN_HEALTHY_PEER_RATE: f64 = 256.0 * 1024.0;
// Ignore submillisecond handoff noise; the accumulated window is checked too.
const MEANINGFUL_LOCAL_WAIT: Duration = Duration::from_millis(1);

struct PeerRate {
    rate: f64,
    context: PeerContext,
    recorded_at: Instant,
}

/// Bounded, operation-local history. Match generation, actual concurrency, and
/// own transfer width exactly: rates from a different controller regime are not
/// evidence that this particular flow is a straggler.
#[derive(Default)]
pub(crate) struct PeerRates(Mutex<VecDeque<PeerRate>>);

impl PeerRates {
    pub fn record_with_context(
        &self,
        bytes: u64,
        network: Duration,
        consumer: Duration,
        context: PeerContext,
    ) {
        self.record_at(bytes, network, consumer, context, Instant::now());
    }

    fn record_at(
        &self,
        bytes: u64,
        network: Duration,
        consumer: Duration,
        context: PeerContext,
        now: Instant,
    ) {
        let context = context.normalized();
        let mut rates = self.0.lock().unwrap_or_else(|error| error.into_inner());
        rates
            .retain(|sample| now.saturating_duration_since(sample.recorded_at) <= PEER_HISTORY_TTL);
        if !context.has_independent_peers()
            || bytes < 1024 * 1024
            || network < Duration::from_millis(250)
            || consumer > network / 3
        {
            return;
        }
        let rate = bytes as f64 / network.as_secs_f64();
        if rate < MIN_HEALTHY_PEER_RATE {
            return;
        }
        if rates.len() == PEER_HISTORY_LIMIT {
            rates.pop_front();
        }
        rates.push_back(PeerRate {
            rate,
            context,
            recorded_at: now,
        });
    }

    pub fn baseline_for(&self, context: PeerContext) -> Option<f64> {
        self.baseline_at(context, Instant::now())
    }

    fn baseline_at(&self, context: PeerContext, now: Instant) -> Option<f64> {
        let context = context.normalized();
        let mut history = self.0.lock().unwrap_or_else(|error| error.into_inner());
        history
            .retain(|sample| now.saturating_duration_since(sample.recorded_at) <= PEER_HISTORY_TTL);
        if !context.has_independent_peers() {
            return None;
        }
        let mut rates = history
            .iter()
            .filter(|sample| sample.context == context)
            .map(|sample| sample.rate)
            .collect::<Vec<_>>();
        if rates.len() < 3 {
            return None;
        }
        rates.sort_by(f64::total_cmp);
        Some(rates[rates.len() / 2])
    }

    // Compatibility for older fixtures only. Production callers must supply
    // their actual context rather than inventing independent peers.
    #[cfg(test)]
    pub fn record(&self, bytes: u64, network: Duration, consumer: Duration) {
        self.record_with_context(bytes, network, consumer, PeerContext::new(2, 1, 0));
    }

    #[cfg(test)]
    pub fn baseline(&self) -> Option<f64> {
        self.baseline_for(PeerContext::new(2, 1, 0))
    }
}

fn consumer_backpressured(network: Duration, local: Duration) -> bool {
    local >= MEANINGFUL_LOCAL_WAIT && local > network / 3
}

/// Network-time windows shared by upload and download. Each observation is a
/// delta excluding observer/controller work and local consumer time. One large
/// delta can close only one window, never manufacture two independent windows.
#[derive(Default)]
pub(crate) struct SlowPeerTracker {
    context: Option<PeerContext>,
    bytes: u64,
    network: Duration,
    local: Duration,
    slow_windows: u8,
    decided: bool,
}

impl SlowPeerTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    #[allow(clippy::too_many_arguments)]
    pub fn observe(
        &mut self,
        peers: &PeerRates,
        context: PeerContext,
        bytes: u64,
        network: Duration,
        local: Duration,
        window: Duration,
        pressured: bool,
    ) -> bool {
        let context = context.normalized();
        if !context.has_independent_peers() || pressured || consumer_backpressured(network, local) {
            self.reset();
            return false;
        }
        if self.context.is_some_and(|previous| previous != context) {
            self.reset();
            self.context = Some(context);
            // The caller may have noticed the change only at the end of this
            // delta. Do not attribute it to either concurrency regime.
            return false;
        }
        self.context = Some(context);
        if self.decided {
            return false;
        }
        self.bytes = self.bytes.saturating_add(bytes);
        self.network = self.network.saturating_add(network);
        self.local = self.local.saturating_add(local);
        if consumer_backpressured(self.network, self.local) {
            self.reset();
            return false;
        }
        if self.network.is_zero() || self.network < window {
            return false;
        }
        let rate = self.bytes as f64 / self.network.as_secs_f64();
        self.bytes = 0;
        self.network = Duration::ZERO;
        self.local = Duration::ZERO;
        if peers
            .baseline_for(context)
            .is_some_and(|baseline| rate < baseline / 8.0)
        {
            self.slow_windows += 1;
        } else {
            self.slow_windows = 0;
        }
        self.decided = self.slow_windows >= 2;
        self.decided
    }
}

pub(crate) enum StreamReadFailure {
    Local(StateError),
    Idle,
    SlowPeer,
}

impl StreamReadFailure {
    pub fn message(&self) -> &'static str {
        match self {
            Self::Local(_) => "stream consumer failed",
            Self::Idle => "pack stream made no network progress for 60 seconds",
            Self::SlowPeer => "pack stream is persistently slower than healthy peers",
        }
    }
}

pub(crate) fn observe(
    observer: Option<&(dyn DownloadObserver + '_)>,
    diagnostics: &StreamTransferDiagnostics,
) -> Result<()> {
    if let Some(observer) = observer {
        observer.transfer_progress_with_diagnostics(diagnostics.attempt_bytes, diagnostics)?;
    }
    Ok(())
}

struct PeerObservation<C> {
    context: C,
    io_pressure: fn() -> bool,
    pressure_interval: Duration,
    pressure_sampled_at: Instant,
    current_pressure: bool,
    current: PeerContext,
    changed: bool,
    pressured: bool,
}

impl<C: Fn() -> PeerContext> PeerObservation<C> {
    fn new(context: C, io_pressure: fn() -> bool, pressure_interval: Duration) -> Self {
        let current = context().normalized();
        let current_pressure = io_pressure();
        Self {
            context,
            io_pressure,
            pressure_interval,
            pressure_sampled_at: Instant::now(),
            current_pressure,
            current,
            changed: false,
            pressured: current_pressure,
        }
    }

    fn sample(&mut self) {
        let current = (self.context)().normalized();
        self.changed |= self.current != current;
        self.current = current;
        // Context counters are cheap enough for every handoff; /proc reads are
        // not. Pending-I/O ticks still refresh this cache without an observer.
        if self.pressure_sampled_at.elapsed() >= self.pressure_interval {
            self.current_pressure = (self.io_pressure)();
            self.pressure_sampled_at = Instant::now();
        }
        self.pressured |= self.current_pressure;
    }

    fn begin_read(&mut self) {
        self.changed = false;
        self.pressured = false;
        self.sample();
    }
}

async fn observed_wait<T>(
    future: impl std::future::Future<Output = T>,
    observer: Option<&(dyn DownloadObserver + '_)>,
    diagnostics: &StreamTransferDiagnostics,
    network: bool,
    interval: Duration,
    peer_observation: &mut PeerObservation<impl Fn() -> PeerContext + Send>,
) -> Result<(T, Duration)> {
    tokio::pin!(future);
    let started = Instant::now();
    let mut local_work = Duration::ZERO;
    // A stack-pinned timer avoids allocating an Interval for every small pipe
    // read/decoder handoff. Ready I/O wins without ever polling the timer. Keep
    // ticking even without an observer so pressure caps can clear while blocked.
    let tick = tokio::time::sleep(interval);
    tokio::pin!(tick);
    loop {
        tokio::select! {
            biased;
            result = &mut future => {
                return Ok((result, started.elapsed().saturating_sub(local_work)));
            },
            _ = &mut tick => {
                let observing = Instant::now();
                let elapsed = micros(started.elapsed().saturating_sub(local_work));
                peer_observation.sample();
                if observer.is_some() {
                    let mut snapshot = diagnostics.clone();
                    if network {
                        snapshot.phase = "waiting_network".into();
                        snapshot.network_wait_us = snapshot.network_wait_us.saturating_add(elapsed);
                    } else {
                        snapshot.phase = "waiting_consumer".into();
                        snapshot.consumer_us = snapshot.consumer_us.saturating_add(elapsed);
                    }
                    observe(observer, &snapshot)?;
                }
                tick.as_mut().reset(tokio::time::Instant::now() + interval);
                local_work = local_work.saturating_add(observing.elapsed());
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn read_attempt(
    reader: &mut (impl AsyncRead + Unpin),
    consumer: &dyn DownloadStreamConsumer,
    observer: Option<&(dyn DownloadObserver + '_)>,
    diagnostics: &mut StreamTransferDiagnostics,
    peers: &PeerRates,
    policy: StreamPolicy,
    allow_slow_restart: bool,
    wire_bytes: &AtomicU64,
    context: impl Fn() -> PeerContext + Send,
) -> std::result::Result<u64, StreamReadFailure> {
    let mut buffer = vec![0u8; 256 * 1024];
    let mut last_observation = Instant::now();
    let mut peers_observed =
        PeerObservation::new(context, policy.io_pressure, policy.observation_interval);
    let mut slow = SlowPeerTracker::new();
    loop {
        peers_observed.begin_read();
        let (read, receive) = observed_wait(
            tokio::time::timeout(policy.idle_timeout, reader.read(&mut buffer)),
            observer,
            diagnostics,
            true,
            policy.observation_interval,
            &mut peers_observed,
        )
        .await
        .map_err(StreamReadFailure::Local)?;
        peers_observed.sample();
        diagnostics.network_wait_us = diagnostics.network_wait_us.saturating_add(micros(receive));
        let count = read
            .map_err(|_| StreamReadFailure::Idle)?
            .map_err(|error| StreamReadFailure::Local(error.into()))?;
        if count == 0 {
            return Ok(diagnostics.attempt_bytes);
        }
        diagnostics.attempt_bytes = diagnostics.attempt_bytes.saturating_add(count as u64);
        diagnostics.received_bytes = diagnostics.received_bytes.saturating_add(count as u64);
        wire_bytes.fetch_add(count as u64, Ordering::Relaxed);
        let (accepted, consumed) = observed_wait(
            consumer.write(&buffer[..count]),
            observer,
            diagnostics,
            false,
            policy.observation_interval,
            &mut peers_observed,
        )
        .await
        .map_err(StreamReadFailure::Local)?;
        peers_observed.sample();
        diagnostics.consumer_us = diagnostics.consumer_us.saturating_add(micros(consumed));
        accepted.map_err(StreamReadFailure::Local)?;
        if last_observation.elapsed() >= Duration::from_millis(500) {
            diagnostics.phase = "receiving".into();
            observe(observer, diagnostics).map_err(StreamReadFailure::Local)?;
            last_observation = Instant::now();
        }
        if peers_observed.changed || peers_observed.pressured {
            // A read spanning a context/pressure transition has no comparable
            // network clock. Start fresh rather than comparing mixed regimes.
            slow.reset();
        } else if allow_slow_restart
            && slow.observe(
                peers,
                peers_observed.current,
                count as u64,
                receive,
                consumed,
                policy.slow_window,
                false,
            )
        {
            return Err(StreamReadFailure::SlowPeer);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use tokio::io::AsyncWriteExt;

    struct Consumer {
        delay: Duration,
        reject: bool,
        bytes: AtomicU64,
        writing: AtomicBool,
        release: Option<tokio::sync::Notify>,
    }
    #[async_trait::async_trait]
    impl DownloadStreamConsumer for Consumer {
        async fn restart(&self) -> Result<()> {
            Ok(())
        }
        async fn write(&self, bytes: &[u8]) -> Result<()> {
            self.writing.store(true, Ordering::Relaxed);
            if let Some(release) = &self.release {
                release.notified().await;
            } else if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            self.writing.store(false, Ordering::Relaxed);
            if self.reject {
                return Err(StateError::Integrity("fixture integrity failure".into()));
            }
            self.bytes.fetch_add(bytes.len() as u64, Ordering::Relaxed);
            Ok(())
        }
        async fn finish(&self) -> Result<()> {
            Ok(())
        }
    }

    fn consumer(delay: Duration) -> Consumer {
        Consumer {
            delay,
            reject: false,
            bytes: AtomicU64::new(0),
            writing: AtomicBool::new(false),
            release: None,
        }
    }

    fn no_pressure() -> bool {
        false
    }

    fn pressure() -> bool {
        true
    }

    fn solo() -> PeerContext {
        PeerContext::new(1, 1, 7)
    }

    fn concurrent() -> PeerContext {
        PeerContext::new(16, 1, 7)
    }

    fn policy() -> StreamPolicy {
        StreamPolicy {
            idle_timeout: Duration::from_millis(50),
            slow_window: Duration::from_millis(25),
            observation_interval: Duration::from_millis(5),
            io_pressure: no_pressure,
            ..StreamPolicy::default()
        }
    }

    fn healthy_peers(context: PeerContext) -> PeerRates {
        let peers = PeerRates::default();
        for _ in 0..3 {
            peers.record_with_context(
                8 * 1024 * 1024,
                Duration::from_secs(1),
                Duration::ZERO,
                context,
            );
        }
        peers
    }

    #[test]
    fn older_diagnostics_default_new_numeric_controller_fields() {
        let diagnostics: StreamTransferDiagnostics =
            serde_json::from_str(r#"{"attempt":2,"phase":"receiving"}"#).unwrap();
        assert_eq!(diagnostics.queued_transfer_requests, 0);
        assert_eq!(diagnostics.controller_epoch, 0);
        assert_eq!(diagnostics.attempt, 2);
    }

    #[tokio::test]
    async fn network_idle_is_bounded_and_partial_wire_bytes_are_counted() {
        let (mut writer, mut reader) = tokio::io::duplex(64);
        writer.write_all(b"partial").await.unwrap();
        let mut metrics = StreamTransferDiagnostics::default();
        let wire = AtomicU64::new(0);
        let result = read_attempt(
            &mut reader,
            &consumer(Duration::ZERO),
            None,
            &mut metrics,
            &PeerRates::default(),
            policy(),
            true,
            &wire,
            solo,
        )
        .await;
        assert!(matches!(result, Err(StreamReadFailure::Idle)));
        assert_eq!(metrics.attempt_bytes, 7);
        assert_eq!(wire.load(Ordering::Relaxed), 7);
        drop(writer);
    }

    #[tokio::test]
    async fn local_backpressure_longer_than_the_idle_timeout_is_not_a_network_stall() {
        let (mut writer, mut reader) = tokio::io::duplex(64);
        writer.write_all(b"verified").await.unwrap();
        writer.shutdown().await.unwrap();
        let mut metrics = StreamTransferDiagnostics::default();
        let wire = AtomicU64::new(0);
        let result = read_attempt(
            &mut reader,
            &consumer(Duration::from_millis(150)),
            None,
            &mut metrics,
            &PeerRates::default(),
            policy(),
            true,
            &wire,
            solo,
        )
        .await;
        assert!(matches!(result, Ok(8)));
        assert!(metrics.consumer_us >= 100_000);
        assert!(metrics.network_wait_us < metrics.consumer_us);
    }

    #[tokio::test]
    async fn integrity_errors_remain_local_and_must_not_be_retried_as_network_faults() {
        let (mut writer, mut reader) = tokio::io::duplex(64);
        writer.write_all(b"bad").await.unwrap();
        let reject = Consumer {
            reject: true,
            ..consumer(Duration::ZERO)
        };
        let mut metrics = StreamTransferDiagnostics::default();
        let result = read_attempt(
            &mut reader,
            &reject,
            None,
            &mut metrics,
            &PeerRates::default(),
            policy(),
            true,
            &AtomicU64::new(0),
            solo,
        )
        .await;
        assert!(matches!(
            result,
            Err(StreamReadFailure::Local(StateError::Integrity(_)))
        ));
    }

    #[tokio::test]
    async fn permission_denied_read_errors_remain_local() {
        struct DeniedReader;
        impl AsyncRead for DeniedReader {
            fn poll_read(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
                _: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                std::task::Poll::Ready(Err(std::io::Error::from(
                    std::io::ErrorKind::PermissionDenied,
                )))
            }
        }
        let result = read_attempt(
            &mut DeniedReader,
            &consumer(Duration::ZERO),
            None,
            &mut StreamTransferDiagnostics::default(),
            &healthy_peers(concurrent()),
            policy(),
            true,
            &AtomicU64::new(0),
            concurrent,
        )
        .await;
        assert!(matches!(
            result,
            Err(StreamReadFailure::Local(StateError::Io(error)))
                if error.kind() == std::io::ErrorKind::PermissionDenied
        ));
    }

    #[test]
    fn context_callback_needs_send_but_not_sync() {
        fn assert_send<T: Send>(_: &T) {}

        let calls = std::cell::Cell::new(0);
        let context = move || {
            calls.set(calls.get() + 1);
            concurrent()
        };
        let (_writer, mut reader) = tokio::io::duplex(64);
        let target = consumer(Duration::ZERO);
        let mut metrics = StreamTransferDiagnostics::default();
        let peers = PeerRates::default();
        let wire = AtomicU64::new(0);
        let read = read_attempt(
            &mut reader,
            &target,
            None,
            &mut metrics,
            &peers,
            policy(),
            true,
            &wire,
            context,
        );
        assert_send(&read);
    }

    #[test]
    fn peer_history_is_bounded_and_rejects_backpressured_samples() {
        let rates = PeerRates::default();
        for _ in 0..3 {
            rates.record(1024 * 1024, Duration::from_secs(1), Duration::from_secs(1));
        }
        assert!(rates.baseline().is_none());
        for _ in 0..32 {
            rates.record(1024 * 1024, Duration::from_secs(1), Duration::ZERO);
        }
        assert_eq!(rates.0.lock().unwrap().len(), 16);
        assert_eq!(rates.baseline(), Some(1024.0 * 1024.0));
    }

    #[test]
    fn pressure_probes_are_cached_on_handoffs_and_remain_latched_for_the_delta() {
        static PROBES: AtomicUsize = AtomicUsize::new(0);
        fn probe() -> bool {
            PROBES.fetch_add(1, Ordering::Relaxed) == 0
        }

        PROBES.store(0, Ordering::Relaxed);
        let interval = Duration::from_secs(60);
        let mut sampled = PeerObservation::new(concurrent, probe, interval);
        for _ in 0..1024 {
            sampled.sample();
        }
        assert_eq!(PROBES.load(Ordering::Relaxed), 1);
        assert!(sampled.pressured);
        sampled.pressure_sampled_at = Instant::now() - interval;
        sampled.sample();
        assert_eq!(PROBES.load(Ordering::Relaxed), 2);
        assert!(!sampled.current_pressure);
        assert!(sampled.pressured);
        sampled.begin_read();
        assert!(!sampled.pressured);
    }

    #[test]
    fn peer_context_normalizes_counts_without_inventing_independent_peers() {
        assert_eq!(PeerContext::new(0, 0, 9), PeerContext::new(1, 1, 9));
        assert_eq!(PeerContext::new(4, 0, 9), PeerContext::new(4, 1, 9));
        assert_eq!(PeerContext::new(2, 16, 9), PeerContext::new(2, 2, 9));
        assert!(!PeerContext::new(0, 16, 9).has_independent_peers());
    }

    #[test]
    fn peer_history_requires_three_credible_samples_in_exactly_the_same_context() {
        let peers = PeerRates::default();
        let context = concurrent();
        for _ in 0..3 {
            peers.record_with_context(
                1024 * 1024 - 1,
                Duration::from_secs(1),
                Duration::ZERO,
                context,
            );
            peers.record_with_context(
                1024 * 1024,
                Duration::from_millis(249),
                Duration::ZERO,
                context,
            );
            peers.record_with_context(1024 * 1024, Duration::from_secs(8), Duration::ZERO, context);
        }
        assert!(peers.baseline_for(context).is_none());
        for _ in 0..2 {
            peers.record_with_context(
                1024 * 1024,
                Duration::from_millis(250),
                Duration::ZERO,
                context,
            );
        }
        assert!(peers.baseline_for(context).is_none());
        peers.record_with_context(
            1024 * 1024,
            Duration::from_millis(250),
            Duration::ZERO,
            context,
        );
        assert_eq!(peers.baseline_for(context), Some(4.0 * 1024.0 * 1024.0));
        for other in [
            solo(),
            PeerContext::new(8, 1, 7),
            PeerContext::new(16, 2, 7),
            PeerContext::new(16, 1, 8),
        ] {
            assert!(peers.baseline_for(other).is_none());
        }
    }

    #[test]
    fn peer_history_expires_old_samples_without_wall_clock_sleeps() {
        let peers = PeerRates::default();
        let context = concurrent();
        let now = Instant::now();
        for _ in 0..3 {
            peers.record_at(
                1024 * 1024,
                Duration::from_secs(1),
                Duration::ZERO,
                context,
                now,
            );
        }
        assert!(peers.baseline_at(context, now + PEER_HISTORY_TTL).is_some());
        let expired = now + PEER_HISTORY_TTL + Duration::from_nanos(1);
        assert!(peers.baseline_at(context, expired).is_none());
        assert!(peers.0.lock().unwrap().is_empty());
        peers.record_at(
            1024 * 1024,
            Duration::from_secs(1),
            Duration::ZERO,
            context,
            expired,
        );
        assert!(peers.baseline_at(context, expired).is_none());
    }

    fn slow_window(tracker: &mut SlowPeerTracker, peers: &PeerRates, context: PeerContext) -> bool {
        tracker.observe(
            peers,
            context,
            64 * 1024,
            Duration::from_secs(1),
            Duration::ZERO,
            Duration::from_secs(1),
            false,
        )
    }

    #[test]
    fn tracker_accumulates_two_complete_network_windows_and_emits_once() {
        let peers = healthy_peers(concurrent());
        let mut tracker = SlowPeerTracker::new();
        for _ in 0..5 {
            assert!(!tracker.observe(
                &peers,
                concurrent(),
                64 * 1024,
                Duration::from_secs(10),
                Duration::ZERO,
                Duration::from_secs(30),
                false,
            ));
        }
        assert!(tracker.observe(
            &peers,
            concurrent(),
            64 * 1024,
            Duration::from_secs(10),
            Duration::ZERO,
            Duration::from_secs(30),
            false,
        ));
        for _ in 0..4 {
            assert!(!slow_window(&mut tracker, &peers, concurrent()));
        }
        tracker.reset();
        assert!(!slow_window(&mut tracker, &peers, concurrent()));
        assert!(slow_window(&mut tracker, &peers, concurrent()));
    }

    #[test]
    fn one_large_delta_cannot_manufacture_two_slow_windows() {
        let peers = healthy_peers(concurrent());
        let mut tracker = SlowPeerTracker::default();
        assert!(!tracker.observe(
            &peers,
            concurrent(),
            64 * 1024,
            Duration::from_secs(120),
            Duration::ZERO,
            Duration::from_secs(30),
            false,
        ));
        assert!(tracker.observe(
            &peers,
            concurrent(),
            64 * 1024,
            Duration::from_secs(30),
            Duration::ZERO,
            Duration::from_secs(30),
            false,
        ));
    }

    #[test]
    fn uniform_shared_slowness_and_nonconsecutive_outliers_are_not_stragglers() {
        let peers = PeerRates::default();
        for _ in 0..3 {
            peers.record_with_context(
                1024 * 1024,
                Duration::from_secs(4),
                Duration::ZERO,
                concurrent(),
            );
        }
        let mut tracker = SlowPeerTracker::new();
        for _ in 0..4 {
            assert!(!tracker.observe(
                &peers,
                concurrent(),
                256 * 1024,
                Duration::from_secs(1),
                Duration::ZERO,
                Duration::from_secs(1),
                false,
            ));
        }
        let peers = healthy_peers(concurrent());
        assert!(!slow_window(&mut tracker, &peers, concurrent()));
        assert!(!tracker.observe(
            &peers,
            concurrent(),
            8 * 1024 * 1024,
            Duration::from_secs(1),
            Duration::ZERO,
            Duration::from_secs(1),
            false,
        ));
        assert!(!slow_window(&mut tracker, &peers, concurrent()));
        assert!(slow_window(&mut tracker, &peers, concurrent()));
    }

    #[test]
    fn concurrency_generation_and_transfer_width_changes_reset_the_sample_clock() {
        let peers = healthy_peers(concurrent());
        let mut tracker = SlowPeerTracker::new();
        assert!(!slow_window(&mut tracker, &peers, concurrent()));
        for _ in 0..3 {
            assert!(!slow_window(&mut tracker, &peers, solo()));
        }
        // Returning to sixteen active slots cannot carry the earlier window.
        assert!(!slow_window(&mut tracker, &peers, concurrent()));
        for other in [
            PeerContext::new(8, 1, 7),
            PeerContext::new(16, 2, 7),
            PeerContext::new(16, 1, 8),
        ] {
            for _ in 0..3 {
                assert!(!slow_window(&mut tracker, &peers, other));
            }
        }
        // The transition delta itself is discarded, then two stable windows
        // are required in the matching controller/concurrency regime.
        assert!(!slow_window(&mut tracker, &peers, concurrent()));
        assert!(!slow_window(&mut tracker, &peers, concurrent()));
        assert!(slow_window(&mut tracker, &peers, concurrent()));
    }

    #[test]
    fn solo_samples_cannot_become_a_sixteen_slot_peer_baseline_or_restart_a_group() {
        let peers = healthy_peers(solo());
        let mut tracker = SlowPeerTracker::new();
        for _ in 0..4 {
            assert!(!slow_window(&mut tracker, &peers, concurrent()));
        }
        let peers = healthy_peers(concurrent());
        for group in [solo(), PeerContext::new(16, 16, 7)] {
            for _ in 0..4 {
                assert!(!slow_window(&mut tracker, &peers, group));
            }
        }
    }

    #[test]
    fn pressure_and_meaningful_consumer_wait_discard_the_whole_slow_streak() {
        let peers = healthy_peers(concurrent());
        let mut tracker = SlowPeerTracker::new();
        assert!(!slow_window(&mut tracker, &peers, concurrent()));
        assert!(!tracker.observe(
            &peers,
            concurrent(),
            64 * 1024,
            Duration::from_secs(1),
            Duration::ZERO,
            Duration::from_secs(1),
            true,
        ));
        assert!(!slow_window(&mut tracker, &peers, concurrent()));
        assert!(!tracker.observe(
            &peers,
            concurrent(),
            64 * 1024,
            Duration::from_secs(1),
            Duration::from_millis(334),
            Duration::from_secs(1),
            false,
        ));
        assert!(!slow_window(&mut tracker, &peers, concurrent()));
        assert!(slow_window(&mut tracker, &peers, concurrent()));
        assert_eq!(peers.0.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn live_diagnostics_distinguish_network_wait_from_consumer_wait() {
        #[derive(Default)]
        struct Observer(Mutex<Vec<StreamTransferDiagnostics>>);
        impl DownloadObserver for Observer {
            fn transfer_progress_with_diagnostics(
                &self,
                _: u64,
                metrics: &StreamTransferDiagnostics,
            ) -> Result<()> {
                self.0.lock().unwrap().push(metrics.clone());
                Ok(())
            }
        }
        let (mut writer, mut reader) = tokio::io::duplex(64);
        let observer = Observer::default();
        let target = consumer(Duration::from_millis(100));
        let sending = async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            writer.write_all(b"data").await.unwrap();
            writer.shutdown().await.unwrap();
        };
        let mut metrics = StreamTransferDiagnostics::default();
        let rates = PeerRates::default();
        let wire = AtomicU64::new(0);
        let read = read_attempt(
            &mut reader,
            &target,
            Some(&observer),
            &mut metrics,
            &rates,
            StreamPolicy {
                idle_timeout: Duration::from_secs(2),
                observation_interval: Duration::from_millis(10),
                ..policy()
            },
            false,
            &wire,
            solo,
        );
        let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(read, sending)
        })
        .await
        .expect("bounded diagnostic fixture");
        assert!(matches!(result, Ok(4)));
        let observations = observer.0.lock().unwrap();
        assert!(observations
            .iter()
            .any(|m| m.phase == "waiting_network" && m.network_wait_us > 0));
        assert!(observations
            .iter()
            .any(|m| m.phase == "waiting_consumer" && m.consumer_us > 0));
    }

    #[tokio::test]
    async fn context_is_refreshed_while_consumer_is_pending_without_an_observer() {
        let (mut writer, mut reader) = tokio::io::duplex(64);
        writer.write_all(b"data").await.unwrap();
        writer.shutdown().await.unwrap();
        let target = Consumer {
            release: Some(tokio::sync::Notify::new()),
            ..consumer(Duration::ZERO)
        };
        let during_write = AtomicUsize::new(0);
        let sampled = tokio::sync::Notify::new();
        let mut diagnostics = StreamTransferDiagnostics::default();
        let peers = PeerRates::default();
        let wire = AtomicU64::new(0);
        let read = read_attempt(
            &mut reader,
            &target,
            None,
            &mut diagnostics,
            &peers,
            policy(),
            false,
            &wire,
            || {
                if target.writing.load(Ordering::Relaxed)
                    && during_write.fetch_add(1, Ordering::Relaxed) + 1 >= 3
                {
                    sampled.notify_one();
                }
                solo()
            },
        );
        let release = async {
            sampled.notified().await;
            target.release.as_ref().unwrap().notify_one();
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(read, release)
        })
        .await
        .expect("bounded pending consumer fixture");
        assert!(matches!(result, Ok(4)));
        assert!(during_write.load(Ordering::Relaxed) >= 3);
    }

    #[tokio::test]
    async fn observation_and_context_callback_work_are_not_network_time() {
        struct SlowObserver<'a> {
            work: AtomicU64,
            complete: &'a tokio::sync::Notify,
        }
        impl DownloadObserver for SlowObserver<'_> {
            fn transfer_progress_with_diagnostics(
                &self,
                _: u64,
                _: &StreamTransferDiagnostics,
            ) -> Result<()> {
                let started = Instant::now();
                std::thread::sleep(Duration::from_millis(10));
                self.work
                    .fetch_add(micros(started.elapsed()), Ordering::Relaxed);
                self.complete.notify_one();
                Ok(())
            }
        }
        let callback_work = AtomicU64::new(0);
        let mut sampled = PeerObservation::new(
            || {
                let started = Instant::now();
                std::thread::sleep(Duration::from_millis(10));
                callback_work.fetch_add(micros(started.elapsed()), Ordering::Relaxed);
                concurrent()
            },
            no_pressure,
            Duration::from_millis(5),
        );
        callback_work.store(0, Ordering::Relaxed);
        let complete = tokio::sync::Notify::new();
        let observer = SlowObserver {
            work: AtomicU64::new(0),
            complete: &complete,
        };
        let started = Instant::now();
        let ((), network) = tokio::time::timeout(
            Duration::from_secs(1),
            observed_wait(
                complete.notified(),
                Some(&observer),
                &StreamTransferDiagnostics::default(),
                true,
                Duration::from_millis(5),
                &mut sampled,
            ),
        )
        .await
        .expect("bounded observation fixture")
        .unwrap();
        let excluded = Duration::from_micros(
            callback_work.load(Ordering::Relaxed) + observer.work.load(Ordering::Relaxed),
        );
        assert!(excluded >= Duration::from_millis(20));
        assert!(started.elapsed().saturating_sub(network) >= excluded);
    }

    #[tokio::test]
    async fn pending_network_refreshes_context_without_an_observer_and_cancel_drops_the_timer() {
        let (_writer, mut reader) = tokio::io::duplex(64);
        let target = consumer(Duration::ZERO);
        let mut metrics = StreamTransferDiagnostics::default();
        let peers = PeerRates::default();
        let wire = AtomicU64::new(0);
        let calls = AtomicUsize::new(0);
        let sampled = tokio::sync::Notify::new();
        let mut read = Box::pin(read_attempt(
            &mut reader,
            &target,
            None,
            &mut metrics,
            &peers,
            StreamPolicy {
                idle_timeout: Duration::from_secs(2),
                ..policy()
            },
            false,
            &wire,
            || {
                if calls.fetch_add(1, Ordering::Relaxed) + 1 >= 6 {
                    sampled.notify_one();
                }
                solo()
            },
        ));
        tokio::time::timeout(Duration::from_secs(1), async {
            tokio::select! {
                biased;
                _ = &mut read => panic!("the fixture should remain blocked"),
                _ = sampled.notified() => {}
            }
        })
        .await
        .expect("bounded pending network fixture");
        assert!(calls.load(Ordering::Relaxed) >= 6);
        drop(read);
        let cancelled_at = calls.load(Ordering::Relaxed);
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(calls.load(Ordering::Relaxed), cancelled_at);
    }

    async fn slow_fixture(
        allow_restart: bool,
        healthy_peers: bool,
        context: impl Fn() -> PeerContext + Send,
        policy: StreamPolicy,
        consumer_delay: Duration,
    ) -> std::result::Result<u64, StreamReadFailure> {
        let (mut writer, mut reader) = tokio::io::duplex(64);
        let sending = async {
            for _ in 0..6 {
                tokio::time::sleep(Duration::from_millis(10)).await;
                if writer.write_all(b"x").await.is_err() {
                    break;
                }
            }
            let _ = writer.shutdown().await;
        };
        let rates = PeerRates::default();
        if healthy_peers {
            for _ in 0..3 {
                rates.record_with_context(
                    1024 * 1024,
                    Duration::from_secs(1),
                    Duration::ZERO,
                    concurrent(),
                );
            }
        }
        let mut metrics = StreamTransferDiagnostics::default();
        let target = consumer(consumer_delay);
        let wire = AtomicU64::new(0);
        let read = read_attempt(
            &mut reader,
            &target,
            None,
            &mut metrics,
            &rates,
            StreamPolicy {
                idle_timeout: Duration::from_secs(2),
                ..policy
            },
            allow_restart,
            &wire,
            context,
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(read, sending)
        })
        .await
        .expect("bounded slow-peer fixture")
        .0
    }

    #[tokio::test]
    async fn isolated_trickle_is_restartable_but_uniform_slow_links_and_second_attempts_are_not() {
        assert!(matches!(
            slow_fixture(true, true, concurrent, policy(), Duration::ZERO).await,
            Err(StreamReadFailure::SlowPeer)
        ));
        assert!(matches!(
            slow_fixture(true, false, concurrent, policy(), Duration::ZERO).await,
            Ok(6)
        ));
        assert!(matches!(
            slow_fixture(false, true, concurrent, policy(), Duration::ZERO).await,
            Ok(6)
        ));
        assert!(matches!(
            slow_fixture(true, true, solo, policy(), Duration::ZERO).await,
            Ok(6)
        ));
    }

    #[tokio::test]
    async fn io_pressure_and_slow_consumers_do_not_trigger_network_restarts() {
        assert!(matches!(
            slow_fixture(
                true,
                true,
                concurrent,
                StreamPolicy {
                    io_pressure: pressure,
                    ..policy()
                },
                Duration::ZERO,
            )
            .await,
            Ok(6)
        ));
        assert!(matches!(
            slow_fixture(true, true, concurrent, policy(), Duration::from_millis(40),).await,
            Ok(6)
        ));
    }
}
