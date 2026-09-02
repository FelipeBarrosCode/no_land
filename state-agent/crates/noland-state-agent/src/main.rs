use std::sync::Arc;

use noland_state_agent::{AgentConfig, StateAgent};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
        .json()
        .init();

    let config = AgentConfig::from_env();
    tracing::info!(
        instance_id = %config.instance_id,
        image_id = %config.image_id,
        db = %config.paths.db_path.display(),
        socket = %config.paths.rpc_socket.display(),
        "starting noland-state-agent"
    );
    let agent = Arc::new(StateAgent::boot(config)?);
    agent.start_observer();
    agent.recover()?;
    agent.discover()?;
    agent.spawn_background();
    agent.serve_rpc().await?;
    Ok(())
}
