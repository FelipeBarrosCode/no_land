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
}

impl Default for StreamPolicy {
    fn default() -> Self {
        Self {
            idle_timeout: Duration::from_secs(60),
            exit_timeout: Duration::from_secs(30),
            slow_window: Duration::from_secs(30),
            observation_interval: Duration::from_secs(1),
        }
    }
}

/// Bounded, operation-local history. Uniformly slow links are not individual
/// stragglers; require several healthy peers before a speculative restart.
#[derive(Default)]
pub(crate) struct PeerRates(Mutex<VecDeque<f64>>);

impl PeerRates {
    pub fn record(&self, bytes: u64, receive: Duration, consumer: Duration) {
        if bytes < 1024 * 1024 || receive < Duration::from_millis(250) || consumer > receive / 3 {
            return;
        }
        let mut rates = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if rates.len() == 16 {
            rates.pop_front();
        }
        rates.push_back(bytes as f64 / receive.as_secs_f64());
    }

    pub(crate) fn baseline(&self) -> Option<f64> {
        let rates = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if rates.len() < 3 {
            return None;
        }
        let mut rates = rates.iter().copied().collect::<Vec<_>>();
        rates.sort_by(f64::total_cmp);
        Some(rates[rates.len() / 2])
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

async fn observed_wait<T>(
    future: impl std::future::Future<Output = T>,
    observer: Option<&(dyn DownloadObserver + '_)>,
    diagnostics: &StreamTransferDiagnostics,
    network: bool,
    interval: Duration,
) -> Result<T> {
    if observer.is_none() {
        return Ok(future.await);
    }
    tokio::pin!(future);
    let started = Instant::now();
    // A stack-pinned timer avoids allocating an Interval for every small pipe
    // read/decoder handoff. Ready I/O wins without ever polling the timer.
    let tick = tokio::time::sleep(interval);
    tokio::pin!(tick);
    loop {
        tokio::select! {
            biased;
            result = &mut future => return Ok(result),
            _ = &mut tick => {
                let mut snapshot = diagnostics.clone();
                let elapsed = micros(started.elapsed());
                if network {
                    snapshot.phase = "waiting_network".into();
                    snapshot.network_wait_us = snapshot.network_wait_us.saturating_add(elapsed);
                } else {
                    snapshot.phase = "waiting_consumer".into();
                    snapshot.consumer_us = snapshot.consumer_us.saturating_add(elapsed);
                }
                observe(observer, &snapshot)?;
                tick.as_mut().reset(tokio::time::Instant::now() + interval);
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
) -> std::result::Result<u64, StreamReadFailure> {
    let mut buffer = vec![0u8; 256 * 1024];
    let mut last_observation = Instant::now();
    let mut window_bytes = 0u64;
    let mut window_receive = Duration::ZERO;
    let mut window_consumer = Duration::ZERO;
    loop {
        let waiting = Instant::now();
        let read = observed_wait(
            tokio::time::timeout(policy.idle_timeout, reader.read(&mut buffer)),
            observer,
            diagnostics,
            true,
            policy.observation_interval,
        )
        .await
        .map_err(StreamReadFailure::Local)?;
        let receive = waiting.elapsed();
        diagnostics.network_wait_us = diagnostics.network_wait_us.saturating_add(micros(receive));
        window_receive += receive;
        let count = read
            .map_err(|_| StreamReadFailure::Idle)?
            .map_err(|error| StreamReadFailure::Local(error.into()))?;
        if count == 0 {
            return Ok(diagnostics.attempt_bytes);
        }
        diagnostics.attempt_bytes = diagnostics.attempt_bytes.saturating_add(count as u64);
        diagnostics.received_bytes = diagnostics.received_bytes.saturating_add(count as u64);
        wire_bytes.fetch_add(count as u64, Ordering::Relaxed);
        window_bytes = window_bytes.saturating_add(count as u64);
        let processing = Instant::now();
        let accepted = observed_wait(
            consumer.write(&buffer[..count]),
            observer,
            diagnostics,
            false,
            policy.observation_interval,
        )
        .await
        .map_err(StreamReadFailure::Local)?;
        let consumed = processing.elapsed();
        diagnostics.consumer_us = diagnostics.consumer_us.saturating_add(micros(consumed));
        window_consumer += consumed;
        accepted.map_err(StreamReadFailure::Local)?;
        if last_observation.elapsed() >= Duration::from_millis(500) {
            diagnostics.phase = "receiving".into();
            observe(observer, diagnostics).map_err(StreamReadFailure::Local)?;
            last_observation = Instant::now();
        }
        if window_receive >= policy.slow_window {
            let rate = window_bytes as f64 / window_receive.as_secs_f64();
            if allow_slow_restart
                && window_consumer <= window_receive / 3
                && peers
                    .baseline()
                    .is_some_and(|baseline| baseline >= 256.0 * 1024.0 && rate < baseline / 8.0)
            {
                return Err(StreamReadFailure::SlowPeer);
            }
            window_bytes = 0;
            window_receive = Duration::ZERO;
            window_consumer = Duration::ZERO;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    struct Consumer {
        delay: Duration,
        reject: bool,
        bytes: AtomicU64,
    }
    #[async_trait::async_trait]
    impl DownloadStreamConsumer for Consumer {
        async fn restart(&self) -> Result<()> {
            Ok(())
        }
        async fn write(&self, bytes: &[u8]) -> Result<()> {
            tokio::time::sleep(self.delay).await;
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
        }
    }
    fn policy() -> StreamPolicy {
        StreamPolicy {
            idle_timeout: Duration::from_millis(50),
            slow_window: Duration::from_millis(25),
            ..StreamPolicy::default()
        }
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
        )
        .await;
        assert!(matches!(
            result,
            Err(StreamReadFailure::Local(StateError::Integrity(_)))
        ));
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
        );
        assert!(matches!(tokio::join!(read, sending).0, Ok(4)));
        let observations = observer.0.lock().unwrap();
        assert!(observations
            .iter()
            .any(|m| m.phase == "waiting_network" && m.network_wait_us > 0));
        assert!(observations
            .iter()
            .any(|m| m.phase == "waiting_consumer" && m.consumer_us > 0));
    }

    async fn slow_fixture(
        allow_restart: bool,
        healthy_peers: bool,
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
                rates.record(1024 * 1024, Duration::from_secs(1), Duration::ZERO);
            }
        }
        let mut metrics = StreamTransferDiagnostics::default();
        let target = consumer(Duration::ZERO);
        let wire = AtomicU64::new(0);
        let read = read_attempt(
            &mut reader,
            &target,
            None,
            &mut metrics,
            &rates,
            StreamPolicy {
                idle_timeout: Duration::from_secs(2),
                ..policy()
            },
            allow_restart,
            &wire,
        );
        tokio::join!(read, sending).0
    }

    #[tokio::test]
    async fn isolated_trickle_is_restartable_but_uniform_slow_links_and_second_attempts_are_not() {
        assert!(matches!(
            slow_fixture(true, true).await,
            Err(StreamReadFailure::SlowPeer)
        ));
        assert!(matches!(slow_fixture(true, false).await, Ok(6)));
        assert!(matches!(slow_fixture(false, true).await, Ok(6)));
    }
}
