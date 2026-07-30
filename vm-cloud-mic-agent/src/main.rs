mod config;
mod receiver;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use clap::Parser;
use tracing::{info, warn};

use config::ReceiverConfig;
use receiver::Receiver;

const STATUS_PATH: &str = "/run/noland/noland_remote_microphone.status.json";

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
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();

    let mut config = match ReceiverConfig::load(&cli.config) {
        Ok(config) => config,
        Err(error) => {
            warn!(
                "Failed to load config from {}: {error}. Using defaults.",
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
        latency_ms = config.jitter.initial_ms,
        "Starting Noland GStreamer microphone receiver"
    );

    let running = Arc::new(AtomicBool::new(true));
    install_signal_handlers(running.clone());

    let receiver = match Receiver::new(&config, STATUS_PATH, running.clone()) {
        Ok(receiver) => receiver,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };

    if let Err(error) = receiver.run() {
        eprintln!("{error}");
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
            info!("Receiver shutdown signal received");
            signal_running.store(false, Ordering::SeqCst);
        });
    }
}
