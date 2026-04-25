use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    routing::{get, post},
    Router,
};
use tokio::sync::RwLock;
use tracing::{info, warn};

mod audio_output;
mod jitter_buffer;
mod metrics;
mod rtp;
mod session;

use audio_output::{AudioOutput, PulseFallbackAudio};
use metrics::MetricsCollector;
use session::{SessionManager, SessionStartRequest};

/// Shared application state.
pub struct AppState {
    session_manager: RwLock<SessionManager>,
    metrics: MetricsCollector,
    audio_output: Arc<dyn AudioOutput + Send + Sync>,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("cloud_mic_agent=info")
        .init();

    let bind_addr: SocketAddr = std::env::var("CLOUD_MIC_AGENT_BIND")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| "127.0.0.1:34779".parse().unwrap());

    let wg_ip = std::env::var("CLOUD_MIC_WG_IP").ok();

    info!("Cloud Mic Agent starting on {}", bind_addr);
    if let Some(ref ip) = wg_ip {
        info!("WireGuard IP configured: {}", ip);
    }

    // Create audio output (PipeWire preferred, Pulse fallback)
    let audio: Arc<dyn AudioOutput + Send + Sync> =
        match audio_output::PipewireAudio::new("cloud_mic", "Cloud Mic") {
            Ok(pw) => {
                info!("Using PipeWire-native audio output");
                Arc::new(pw)
            }
            Err(e) => {
                warn!("PipeWire init failed ({}), falling back to PulseAudio null-sink", e);
                Arc::new(PulseFallbackAudio::new("cloud_mic", "Cloud Mic").expect("Fallback audio init failed"))
            }
        };

    let state = Arc::new(AppState {
        session_manager: RwLock::new(SessionManager::new(wg_ip)),
        metrics: MetricsCollector::new(),
        audio_output: audio,
    });

    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/status", get(status_handler))
        .route("/metrics", get(metrics_handler))
        .route("/session/start", post(session_start_handler))
        .route("/session/stop", post(session_stop_handler))
        .route("/device/create", post(device_create_handler))
        .route("/device/recreate", post(device_recreate_handler))
        .route("/device/set-default", post(device_set_default_handler))
        .with_state(state.clone());

    // Spawn UDP listener for RTP
    let udp_state = state.clone();
    tokio::spawn(async move {
        if let Err(e) = run_udp_listener(udp_state).await {
            warn!("UDP listener exited: {}", e);
        }
    });

    let listener = tokio::net::TcpListener::bind(bind_addr).await.unwrap();
    info!("HTTP API listening on {}", bind_addr);

    axum::serve(listener, app).await.unwrap();
}

async fn health_handler() -> &'static str {
    "ok"
}

async fn status_handler(state: axum::extract::State<Arc<AppState>>) -> String {
    let session = state.session_manager.read().await;
    let snapshot = state.metrics.snapshot();
    let device_ready = state.audio_output.is_ready().await.unwrap_or(false);

    let response = serde_json::json!({
        "enabled": session.is_active(),
        "deviceReady": device_ready,
        "receivingAudio": snapshot.packets_received_1s > 0,
        "packetLossPercent": state.metrics.packet_loss_percent(),
        "jitterMs": state.metrics.jitter_ms(),
        "bufferDepthMs": state.metrics.buffer_depth_ms(),
        "lastPacketMsAgo": state.metrics.last_packet_ms_ago(),
        "pipewireConnected": state.audio_output.backend_name() == "pipewire",
        "defaultSource": false,
    });

    response.to_string()
}

async fn metrics_handler(state: axum::extract::State<Arc<AppState>>) -> String {
    let m = state.metrics.snapshot();
    serde_json::to_string(&m).unwrap_or_else(|_| "{}".to_string())
}

async fn session_start_handler(
    state: axum::extract::State<Arc<AppState>>,
    axum::Json(payload): axum::Json<SessionStartRequest>,
) -> axum::response::Result<String> {
    let mut session = state.session_manager.write().await;
    match session.start(payload).await {
        Ok(_) => Ok(serde_json::json!({ "status": "started" }).to_string()),
        Err(e) => Err(axum::response::Response::builder()
            .status(400)
            .body(format!("{{\"error\":\"{}\"}}", e))
            .unwrap()
            .into()),
    }
}

async fn session_stop_handler(
    state: axum::extract::State<Arc<AppState>>,
) -> String {
    let mut session = state.session_manager.write().await;
    session.stop().await;
    serde_json::json!({ "status": "stopped" }).to_string()
}

async fn device_create_handler(
    state: axum::extract::State<Arc<AppState>>,
) -> String {
    match state.audio_output.create_or_verify().await {
        Ok(_) => serde_json::json!({ "status": "created" }).to_string(),
        Err(e) => serde_json::json!({ "status": "error", "error": e.to_string() }).to_string(),
    }
}

async fn device_recreate_handler(
    state: axum::extract::State<Arc<AppState>>,
) -> String {
    match state.audio_output.recreate().await {
        Ok(_) => serde_json::json!({ "status": "recreated" }).to_string(),
        Err(e) => serde_json::json!({ "status": "error", "error": e.to_string() }).to_string(),
    }
}

async fn device_set_default_handler(
    state: axum::extract::State<Arc<AppState>>,
) -> String {
    match state.audio_output.set_default().await {
        Ok(_) => serde_json::json!({ "status": "default_set" }).to_string(),
        Err(e) => serde_json::json!({ "status": "error", "error": e.to_string() }).to_string(),
    }
}

async fn run_udp_listener(state: Arc<AppState>) -> Result<(), Box<dyn std::error::Error>> {
    let port = std::env::var("CLOUD_MIC_RTP_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(34778u16);

    let bind_addr = SocketAddr::from(([0, 0, 0, 0], port));
    let socket = tokio::net::UdpSocket::bind(bind_addr).await?;
    info!("RTP UDP listener on {}", bind_addr);

    let mut buf = vec![0u8; 2048];
    loop {
        let (len, peer) = socket.recv_from(&mut buf).await?;

        let session = state.session_manager.read().await;
        if !session.accepts_peer(&peer) {
            continue;
        }
        drop(session);

        if let Some(packet) = rtp::RtpPacket::parse(&buf[..len]) {
            state.metrics.record_packet(&packet);

            // TODO: decode Opus and write to audio output
            // For MVP, just track that we're receiving
        }
    }
}
