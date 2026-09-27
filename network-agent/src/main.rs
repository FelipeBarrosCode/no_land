use std::sync::Arc;

use clap::Parser;
use noland_network_agent::{
    config::Config,
    control,
    probe::udp,
    remote_control::{load_control_secret, RemoteControl},
    shared_state,
    transport::websocket,
    turn_manager::TurnManager,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::parse();
    let thresholds = Arc::new(config.classifier_thresholds());
    let shared = shared_state(config.max_sessions, config.udp_rate_limit);
    let turn_manager = TurnManager::new(
        config.instance_id.clone(),
        env!("CARGO_PKG_VERSION").to_string(),
        config.turn_state_path.clone(),
        config.kernel_wireguard_addr,
        shared.clone(),
    )?;
    let remote_control = load_control_secret(&config.control_secret_path)?
        .map(|secret| RemoteControl::new(config.instance_id.clone(), secret, turn_manager.clone()));

    eprintln!("No Land network agent UDP listening on {}", config.udp_addr);
    eprintln!(
        "No Land network agent WebSocket listening on {}",
        config.ws_addr
    );
    if remote_control.is_none() {
        eprintln!(
            "No Land network agent privileged WebSocket control is disabled: {} is absent",
            config.control_secret_path.display()
        );
    }
    eprintln!(
        "No Land network agent local control listening on {}",
        config.control_socket.display()
    );

    tokio::try_join!(
        udp::run(config.udp_addr, shared.clone()),
        websocket::run(config.ws_addr, shared, thresholds, remote_control),
        control::run(config.control_socket, turn_manager),
    )?;
    Ok(())
}
