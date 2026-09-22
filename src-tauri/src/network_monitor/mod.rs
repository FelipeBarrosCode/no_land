mod probe;
mod reporter;
mod status;

use std::{
    collections::{BTreeMap, VecDeque},
    net::{Ipv6Addr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};

use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::{json, Value};
use tauri::AppHandle;
use tokio::{
    net::UdpSocket,
    sync::{watch, Mutex, RwLock},
    task::JoinHandle,
    time::{interval, sleep, MissedTickBehavior},
};
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};
use tracing::warn;
use uuid::Uuid;

use self::{
    probe::{encode_probe, validate_ack, PACKET_LEN},
    reporter::Reporter,
    status::FailSafeState,
};

const UDP_PORT: u16 = 6201;
const WS_PORT: u16 = 6202;
const PROBE_INTERVAL: Duration = Duration::from_millis(200);
const PROBE_TIMEOUT: Duration = Duration::from_secs(1);
const REPORT_INTERVAL: Duration = Duration::from_secs(1);
const RECONNECT_DELAY: Duration = Duration::from_secs(1);
const MAX_REPORT_SAMPLES: usize = 100;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkMonitorSession {
    pub session_id: Uuid,
    pub target_host: String,
}

#[derive(Clone)]
pub struct NetworkMonitor {
    lifecycle: Arc<Mutex<()>>,
    inner: Arc<Mutex<MonitorState>>,
    latest_stats: Arc<RwLock<Option<Value>>>,
}

#[derive(Default)]
struct MonitorState {
    cancel: Option<watch::Sender<bool>>,
    task: Option<JoinHandle<()>>,
}

impl Default for NetworkMonitor {
    fn default() -> Self {
        Self {
            lifecycle: Arc::new(Mutex::new(())),
            inner: Arc::new(Mutex::new(MonitorState::default())),
            latest_stats: Arc::new(RwLock::new(None)),
        }
    }
}

impl NetworkMonitor {
    pub async fn start(
        &self,
        app: AppHandle,
        target_host: String,
    ) -> Result<NetworkMonitorSession, String> {
        let _lifecycle = self.lifecycle.lock().await;
        self.stop_current().await;

        let target_host = normalize_target_host(&target_host)?;
        let session = NetworkMonitorSession {
            session_id: Uuid::new_v4(),
            target_host,
        };
        let token: [u8; 32] = rand::random();
        let (cancel_tx, cancel_rx) = watch::channel(false);

        *self.latest_stats.write().await = None;
        let task_session = session.clone();
        let latest_stats = self.latest_stats.clone();
        let task = tokio::spawn(async move {
            run_monitor(app, task_session, token, latest_stats, cancel_rx).await;
        });

        let mut state = self.inner.lock().await;
        state.cancel = Some(cancel_tx);
        state.task = Some(task);
        Ok(session)
    }

    pub async fn stop(&self) {
        let _lifecycle = self.lifecycle.lock().await;
        self.stop_current().await;
    }

    pub async fn snapshot(&self) -> Option<Value> {
        self.latest_stats.read().await.clone()
    }

    async fn stop_current(&self) {
        let (cancel, task) = {
            let mut state = self.inner.lock().await;
            (state.cancel.take(), state.task.take())
        };

        if let Some(cancel) = cancel {
            let _ = cancel.send(true);
        }
        if let Some(task) = task {
            if let Err(error) = task.await {
                warn!(%error, "network monitor task ended unexpectedly");
            }
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MeasurementSample {
    sequence: u64,
    rtt_ms: Option<f64>,
    lost: bool,
}

#[derive(Debug)]
enum ConnectedResult {
    Cancelled,
    Disconnected(String),
}

async fn run_monitor(
    app: AppHandle,
    session: NetworkMonitorSession,
    token: [u8; 32],
    latest_stats: Arc<RwLock<Option<Value>>>,
    mut cancel: watch::Receiver<bool>,
) {
    let reporter = Reporter::new(app, session.session_id, latest_stats);
    let ws_url = websocket_url(&session.target_host);
    let mut fail_safe = FailSafeState::new(Instant::now());

    loop {
        if *cancel.borrow() {
            return;
        }

        let udp = match tokio::select! {
            _ = cancelled(&mut cancel) => return,
            result = connect_udp(&session.target_host) => result,
        } {
            Ok(socket) => socket,
            Err(error) => {
                reporter.error(error);
                check_fail_safe(&mut fail_safe, &reporter, Instant::now());
                if !wait_to_retry(&mut cancel).await {
                    return;
                }
                continue;
            }
        };

        let mut websocket = match tokio::select! {
            _ = cancelled(&mut cancel) => return,
            result = connect_async(&ws_url) => result,
        } {
            Ok((websocket, _)) => websocket,
            Err(error) => {
                reporter.error(format!("WebSocket connection failed: {error}"));
                check_fail_safe(&mut fail_safe, &reporter, Instant::now());
                if !wait_to_retry(&mut cancel).await {
                    return;
                }
                continue;
            }
        };

        let auth = Message::Text(
            json!({
                "type": "auth",
                "sessionId": session.session_id,
                "token": hex::encode(token)
            })
            .to_string(),
        );
        let auth_result = tokio::select! {
            _ = cancelled(&mut cancel) => return,
            result = websocket.send(auth) => result,
        };
        if let Err(error) = auth_result {
            reporter.error(format!("WebSocket auth send failed: {error}"));
            check_fail_safe(&mut fail_safe, &reporter, Instant::now());
            if !wait_to_retry(&mut cancel).await {
                return;
            }
            continue;
        }

        match run_connected(
            websocket,
            Some(udp),
            &session,
            &token,
            &reporter,
            &mut fail_safe,
            &mut cancel,
        )
        .await
        {
            ConnectedResult::Cancelled => return,
            ConnectedResult::Disconnected(error) => {
                reporter.error(error);
                check_fail_safe(&mut fail_safe, &reporter, Instant::now());
                if !wait_to_retry(&mut cancel).await {
                    return;
                }
            }
        }
    }
}

async fn run_connected(
    websocket: WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    mut udp: Option<UdpSocket>,
    session: &NetworkMonitorSession,
    token: &[u8; 32],
    reporter: &Reporter,
    fail_safe: &mut FailSafeState,
    cancel: &mut watch::Receiver<bool>,
) -> ConnectedResult {
    let (mut sink, mut stream) = websocket.split();
    let mut sequence = 0_u64;
    let mut pending = BTreeMap::<u64, Instant>::new();
    let mut samples = VecDeque::<MeasurementSample>::new();
    let mut receive_buffer = [0_u8; PACKET_LEN];

    let mut probe_tick = interval(PROBE_INTERVAL);
    probe_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    probe_tick.tick().await;
    let mut report_tick = interval(REPORT_INTERVAL);
    report_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    report_tick.tick().await;
    let mut health_tick = interval(PROBE_INTERVAL);
    health_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    health_tick.tick().await;
    let mut udp_retry_tick = interval(RECONNECT_DELAY);
    udp_retry_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    udp_retry_tick.tick().await;

    loop {
        tokio::select! {
            _ = cancelled(cancel) => return ConnectedResult::Cancelled,
            _ = probe_tick.tick() => {
                let now = Instant::now();
                expire_pending(now, &mut pending, &mut samples);
                let Some(socket) = udp.as_ref() else {
                    continue;
                };

                let current_sequence = sequence;
                sequence = sequence.wrapping_add(1);
                let packet = encode_probe(current_sequence, session.session_id, token);
                let send_result = tokio::select! {
                    _ = cancelled(cancel) => return ConnectedResult::Cancelled,
                    result = socket.send(&packet) => result,
                };
                match send_result {
                    Ok(sent) if sent == PACKET_LEN => {
                        pending.insert(current_sequence, now);
                    }
                    Ok(sent) => {
                        reporter.error(format!("UDP probe send was truncated to {sent} bytes"));
                        udp = None;
                    }
                    Err(error) => {
                        reporter.error(format!("UDP probe send failed: {error}"));
                        udp = None;
                    }
                }
            }
            received = receive_udp(udp.as_ref(), &mut receive_buffer) => {
                match received {
                    Ok(received) => {
                        let now = Instant::now();
                        let Some(ack_sequence) = validate_ack(
                            &receive_buffer[..received],
                            session.session_id,
                            token,
                        ) else {
                            continue;
                        };
                        let Some(sent_at) = pending.remove(&ack_sequence) else {
                            continue;
                        };

                        samples.push_back(MeasurementSample {
                            sequence: ack_sequence,
                            rtt_ms: Some(now.saturating_duration_since(sent_at).as_secs_f64() * 1_000.0),
                            lost: false,
                        });
                        if let Some(transition) = fail_safe.observe_valid_ack(now) {
                            reporter.fail_safe(transition);
                        }
                    }
                    Err(error) => {
                        reporter.error(format!("UDP receive failed: {error}"));
                        udp = None;
                    }
                }
            }
            _ = udp_retry_tick.tick(), if udp.is_none() => {
                let result = tokio::select! {
                    _ = cancelled(cancel) => return ConnectedResult::Cancelled,
                    result = connect_udp(&session.target_host) => result,
                };
                match result {
                    Ok(socket) => udp = Some(socket),
                    Err(error) => reporter.error(error),
                }
            }
            _ = report_tick.tick() => {
                expire_pending(Instant::now(), &mut pending, &mut samples);
                if samples.is_empty() {
                    continue;
                }

                let count = samples.len().min(MAX_REPORT_SAMPLES);
                let report_samples: Vec<_> = samples.drain(..count).collect();
                let report = match serde_json::to_string(&json!({
                    "type": "measurement_report",
                    "sessionId": session.session_id,
                    "samples": report_samples
                })) {
                    Ok(report) => report,
                    Err(error) => {
                        reporter.error(format!("failed to serialize measurement report: {error}"));
                        continue;
                    }
                };
                let send_result = tokio::select! {
                    _ = cancelled(cancel) => return ConnectedResult::Cancelled,
                    result = sink.send(Message::Text(report)) => result,
                };
                if let Err(error) = send_result {
                    return ConnectedResult::Disconnected(format!("WebSocket send failed: {error}"));
                }
            }
            _ = health_tick.tick() => {
                check_fail_safe(fail_safe, reporter, Instant::now());
            }
            incoming = stream.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        reporter.forward_agent_text(text.as_ref()).await;
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        let send_result = tokio::select! {
                            _ = cancelled(cancel) => return ConnectedResult::Cancelled,
                            result = sink.send(Message::Pong(payload)) => result,
                        };
                        if let Err(error) = send_result {
                            return ConnectedResult::Disconnected(format!("WebSocket pong failed: {error}"));
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        return ConnectedResult::Disconnected("WebSocket disconnected".to_string());
                    }
                    Some(Ok(_)) => {}
                    Some(Err(error)) => {
                        return ConnectedResult::Disconnected(format!("WebSocket receive failed: {error}"));
                    }
                }
            }
        }
    }
}

async fn receive_udp(
    socket: Option<&UdpSocket>,
    buffer: &mut [u8; PACKET_LEN],
) -> std::io::Result<usize> {
    match socket {
        Some(socket) => socket.recv(buffer).await,
        None => std::future::pending().await,
    }
}

fn expire_pending(
    now: Instant,
    pending: &mut BTreeMap<u64, Instant>,
    samples: &mut VecDeque<MeasurementSample>,
) {
    let timed_out: Vec<u64> = pending
        .iter()
        .filter_map(|(sequence, sent_at)| {
            (now.saturating_duration_since(*sent_at) >= PROBE_TIMEOUT).then_some(*sequence)
        })
        .collect();

    for sequence in timed_out {
        pending.remove(&sequence);
        samples.push_back(MeasurementSample {
            sequence,
            rtt_ms: None,
            lost: true,
        });
    }
}

fn check_fail_safe(state: &mut FailSafeState, reporter: &Reporter, now: Instant) {
    if let Some(transition) = state.tick(now) {
        reporter.fail_safe(transition);
    }
}

async fn connect_udp(target_host: &str) -> Result<UdpSocket, String> {
    let targets: Vec<SocketAddr> = tokio::net::lookup_host((target_host, UDP_PORT))
        .await
        .map_err(|error| format!("failed to resolve UDP target {target_host}: {error}"))?
        .collect();
    if targets.is_empty() {
        return Err(format!("UDP target {target_host} resolved to no addresses"));
    }

    let mut last_error = None;
    for target in targets {
        let bind_address = if target.is_ipv4() {
            SocketAddr::from(([0, 0, 0, 0], 0))
        } else {
            SocketAddr::from(([0_u16; 8], 0))
        };
        match UdpSocket::bind(bind_address).await {
            Ok(socket) => match socket.connect(target).await {
                Ok(()) => return Ok(socket),
                Err(error) => last_error = Some(error),
            },
            Err(error) => last_error = Some(error),
        }
    }

    Err(match last_error {
        Some(error) => format!("failed to connect UDP probe socket: {error}"),
        None => "failed to connect UDP probe socket".to_string(),
    })
}

fn normalize_target_host(target_host: &str) -> Result<String, String> {
    let target_host = target_host.trim();
    if target_host.is_empty() {
        return Err("network monitor target host must not be empty".to_string());
    }
    if target_host.contains("//") || target_host.contains('/') {
        return Err("network monitor target must be a host without a scheme or path".to_string());
    }

    let unbracketed = if target_host.starts_with('[') && target_host.ends_with(']') {
        target_host[1..target_host.len() - 1].to_string()
    } else if target_host.starts_with('[') || target_host.ends_with(']') {
        return Err("network monitor target has unmatched IPv6 brackets".to_string());
    } else {
        target_host.to_string()
    };

    if unbracketed.is_empty() {
        return Err("network monitor target host must not be empty".to_string());
    }
    if unbracketed.contains(':') && unbracketed.parse::<Ipv6Addr>().is_err() {
        return Err("network monitor target must not include a port".to_string());
    }
    Ok(unbracketed)
}

fn websocket_url(target_host: &str) -> String {
    if target_host.parse::<Ipv6Addr>().is_ok() {
        format!("ws://[{target_host}]:{WS_PORT}")
    } else {
        format!("ws://{target_host}:{WS_PORT}")
    }
}

async fn cancelled(cancel: &mut watch::Receiver<bool>) {
    if *cancel.borrow() {
        return;
    }
    loop {
        if cancel.changed().await.is_err() || *cancel.borrow() {
            return;
        }
    }
}

async fn wait_to_retry(cancel: &mut watch::Receiver<bool>) -> bool {
    tokio::select! {
        _ = cancelled(cancel) => false,
        _ = sleep(RECONNECT_DELAY) => true,
    }
}
