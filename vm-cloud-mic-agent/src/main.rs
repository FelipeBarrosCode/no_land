mod auth_session;
mod config;
mod decoder;
mod jitter;
mod receiver;

use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use tracing::{error, info, warn};

use config::ReceiverConfig;
use receiver::Receiver;

#[derive(Parser)]
#[command(name = "noland-mic-receiver")]
#[command(about = "Noland remote microphone receiver", version)]
struct Cli {
    /// Path to configuration file
    #[arg(short, long, default_value = "/etc/noland/microphone.toml")]
    config: String,

    /// Bind address (overrides config)
    #[arg(long)]
    bind: Option<String>,

    /// UDP port (overrides config)
    #[arg(long)]
    port: Option<u16>,
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();

    // Load configuration
    let mut config = match ReceiverConfig::load(&cli.config) {
        Ok(c) => c,
        Err(e) => {
            warn!(
                "Failed to load config from {}: {e}. Using defaults.",
                cli.config
            );
            ReceiverConfig::default()
        }
    };

    if let Some(bind) = cli.bind {
        config.network.bind_address = bind;
    }
    if let Some(port) = cli.port {
        config.network.port = port;
    }

    info!(
        bind = %config.network.bind_address,
        port = config.network.port,
        "Starting Noland microphone receiver"
    );

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    // Handle SIGTERM/SIGINT on Unix
    #[cfg(unix)]
    {
        let r = r.clone();
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
            info!("Shutdown signal received");
            r.store(false, Ordering::SeqCst);
        });
    }

    // Bind UDP socket
    let bind_addr = format!("{}:{}", config.network.bind_address, config.network.port);
    let socket = match UdpSocket::bind(&bind_addr) {
        Ok(s) => {
            info!(addr = %bind_addr, "UDP socket bound");
            s
        }
        Err(e) => {
            error!("Failed to bind UDP socket on {bind_addr}: {e}");
            std::process::exit(1);
        }
    };

    socket
        .set_read_timeout(Some(Duration::from_millis(10)))
        .ok();

    // Create receiver pipeline
    let mut receiver = Receiver::new(config.audio.clone(), config.jitter.clone());
    let mut stdout = std::io::stdout();

    info!("Receiver initialized. Waiting for microphone packets...");

    // ── Main receive loop ──
    let mut buf = vec![0u8; config.network.maximum_packet_size];

    while running.load(Ordering::SeqCst) {
        // Process all available UDP packets (non-blocking-ish via timeout)
        loop {
            match socket.recv(&mut buf) {
                Ok(n) => {
                    receiver.process_packet(&buf[..n]);
                }
                Err(ref e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    break;
                }
                Err(e) => {
                    warn!("UDP recv error: {e}");
                    break;
                }
            }
        }

        // Drive the jitter buffer / decoder
        receiver.tick(&mut stdout);

        // Drain decoded PCM to stdout (for PipeWire pipe-source)
        receiver.drain_pcm(&mut stdout);

        // Sleep a tiny amount to avoid busy-waiting
        std::thread::sleep(Duration::from_millis(1));
    }

    info!("Receiver shutting down");
    receiver.flush(&mut stdout);
    info!("Noland microphone receiver stopped");
}
