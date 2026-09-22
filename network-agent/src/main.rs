use std::sync::Arc;

use clap::Parser;
use noland_network_agent::{config::Config, probe::udp, shared_state, transport::websocket};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::parse();
    let thresholds = Arc::new(config.classifier_thresholds());
    let shared = shared_state(config.max_sessions, config.udp_rate_limit);

    eprintln!("No Land network agent UDP listening on {}", config.udp_addr);
    eprintln!(
        "No Land network agent WebSocket listening on {}",
        config.ws_addr
    );

    tokio::try_join!(
        udp::run(config.udp_addr, shared.clone()),
        websocket::run(config.ws_addr, shared, thresholds),
    )?;
    Ok(())
}
