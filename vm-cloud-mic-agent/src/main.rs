mod config;
mod receiver;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use clap::Parser;
use tracing::{error, info};

use config::ReceiverConfig;
use receiver::Receiver;

const STATUS_PATH: &str = "/run/noland/noland_remote_microphone.status.json";

#[derive(Parser)]
#[command(name = "noland-mic-receiver")]
#[command(
    about = "Noland RTP/Opus receiver for the persistent PipeWire microphone",
    version
)]
struct Cli {
    /// Path to configuration file
    #[arg(short, long, default_value = "/etc/noland/microphone.toml")]
    config: String,

    /// WireGuard bind address (overrides config)
    #[arg(long)]
    bind: Option<String>,

    /// RTP UDP port (overrides config)
    #[arg(long, alias = "port")]
    rtp_port: Option<u16>,

    /// RTCP UDP port (overrides config)
    #[arg(long)]
    rtcp_port: Option<u16>,

    /// Expected WireGuard peer IP and RTCP report destination (overrides config)
    #[arg(long)]
    expected_peer_ip: Option<String>,

    /// Client UDP port that receives RTCP receiver reports (overrides config)
    #[arg(long)]
    client_rtcp_port: Option<u16>,

    /// Expected RTP SSRC (overrides config)
    #[arg(long)]
    expected_ssrc: Option<u32>,

    /// Session identifier allocated by the authenticated SSH control plane
    #[arg(long)]
    session_id: Option<String>,
}

fn main() {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_target(true)
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let mut config = match ReceiverConfig::load(&cli.config) {
        Ok(config) => config,
        Err(error) => {
            error!(config_path = %cli.config, error = %error, "receiver configuration load failed");
            std::process::exit(2);
        }
    };

    if let Some(bind) = cli.bind {
        config.network.bind_address = bind;
    }
    if let Some(port) = cli.rtp_port {
        config.network.rtp_port = port;
    }
    if let Some(port) = cli.rtcp_port {
        config.network.rtcp_port = port;
    }
    if let Some(peer) = cli.expected_peer_ip {
        config.session.expected_peer_ip = Some(peer);
    }
    if let Some(port) = cli.client_rtcp_port {
        config.session.client_rtcp_port = port;
    }
    if let Some(ssrc) = cli.expected_ssrc {
        config.session.expected_ssrc = Some(ssrc);
    }
    if let Some(session_id) = cli.session_id {
        config.session.session_id = session_id;
    }
    if let Err(error) = config.validate() {
        error!(error = %error, "receiver configuration override validation failed");
        std::process::exit(2);
    }

    info!(
        session_id = %config.session.session_id,
        bind = %config.network.bind_address,
        rtp_port = config.network.rtp_port,
        rtcp_port = config.network.rtcp_port,
        expected_peer_ip = config.session.expected_peer_ip.as_deref(),
        client_rtcp_port = config.session.client_rtcp_port,
        expected_ssrc = config.session.expected_ssrc,
        latency_ms = config.jitter.initial_ms,
        recording_source_fifo = %config.audio.source_fifo_path,
        "starting receiver"
    );

    let running = Arc::new(AtomicBool::new(true));
    install_signal_handlers(running.clone());

    let receiver = match Receiver::new(&config, STATUS_PATH, running.clone()) {
        Ok(receiver) => receiver,
        Err(error) => {
            error!(error = %error, "receiver initialization failed");
            std::process::exit(1);
        }
    };

    if let Err(error) = receiver.run() {
        error!(error = %error, "receiver stopped with an error");
        std::process::exit(1);
    }

    running.store(false, Ordering::SeqCst);
}

fn install_signal_handlers(running: Arc<AtomicBool>) {
    #[cfg(unix)]
    {
        let signal_running = running.clone();
        std::thread::spawn(move || {
            use std::sync::mpsc;
            let (tx, rx) = mpsc::channel();
            let tx2 = tx.clone();
            unsafe {
                signal_hook::low_level::register(signal_hook::consts::SIGTERM, move || {
                    let _ = tx.send(());
                })
                .ok();
                signal_hook::low_level::register(signal_hook::consts::SIGINT, move || {
                    let _ = tx2.send(());
                })
                .ok();
            }
            let _ = rx.recv();
            info!("receiver shutdown signal received");
            signal_running.store(false, Ordering::SeqCst);
        });
    }
}
